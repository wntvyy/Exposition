// Settings in the real app: open from the palette (and Mod+,), switch theme,
// trust a project through the normal trust prompt, see it under Trusted
// folders, revoke it (a job there is refused again), choose a LOCAL Docker
// image for restricted execution (persisted across a relaunch and reported by
// the execution service), refuse invalid tool locations with clear messages,
// accept a real one (discovery reports it), and server-side validation of the
// IPC commands. Screenshots go to test-results/e2e/settings/ and, when
// EXPOSITION_DEV_HOME is set, to $EXPOSITION_DEV_HOME/ui-reference/settings/.
//
// Requires a debug build (dev-only `dev_start_workspace_job`), Python, and
// Docker with the local image `python:3.13-slim` (never pulled).
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { copyFile, mkdir, mkdtemp, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { waitForAsync } from "../wait.mjs";

const IMAGE = "python:3.13-slim";
const VIEWPORT = { width: 1440, height: 900 };
const LAYOUT = JSON.stringify({
  version: 1,
  sidebar: { visible: true, size: 220, activeView: null },
  panel: { visible: false, size: 240, activeView: null },
  auxiliary: { visible: false, size: 320, activeView: null },
  editorSplit: 50,
});

const invoke = (page, cmd, args = {}) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
/** The command's rejection value (e.g. `{ code, message }`), or null if it succeeded. */
const rejection = (page, cmd, args = {}) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a).then(() => null, (e) => e), [cmd, args]);

async function waitFor(what, fn, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  let last;
  for (;;) {
    last = await fn();
    if (last) return last;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

/** Close like a user (WM_CLOSE) so WebView2 flushes localStorage (theme). */
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
  await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 15_000))]);
  await app.close();
}

/** Wait for enter/exit transitions and finite animations to finish. */
async function settle(page) {
  await page.waitForFunction(
    () =>
      !document.querySelector("[data-entering], [data-exiting]") &&
      document.getAnimations().every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity),
    null,
    { timeout: 5000 },
  );
}

async function runCommand(page, assert, query, expected) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByRole("textbox", { name: "Command" });
  await input.waitFor({ state: "visible", timeout: 5000 });
  await input.fill(query);
  const first = page.locator(".wb-palette__item").first();
  await first.waitFor({ state: "visible" });
  assert.match(await first.innerText(), expected, `top palette match for "${query}"`);
  await page.keyboard.press("Enter");
  await input.waitFor({ state: "detached", timeout: 5000 });
}

async function prepare(page) {
  await page.setViewportSize(VIEWPORT);
  await page.evaluate((layout) => localStorage.setItem("exposition.workbench.layout", layout), LAYOUT);
  await page.reload();
  await page.getByTestId("workbench").waitFor({ timeout: 15_000 });
}

async function section(page, name) {
  await page.getByRole("tab", { name, exact: true }).click();
  await page.getByRole("tabpanel", { name }).waitFor();
}

async function setTheme(page, assert, label, expected) {
  await page.getByRole("radiogroup", { name: "Color theme" }).getByRole("radio", { name: label }).click();
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, expected);
  assert.equal(await page.evaluate(() => localStorage.getItem("exposition.theme")), JSON.stringify(label.toLowerCase()));
}

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

/** A Python interpreter that discovery does not pick by default, if there is one. */
function alternatePython(discovered) {
  const local = process.env.LOCALAPPDATA;
  const candidates = local
    ? ["pythoncore-3.12-64", "pythoncore-3.13-64", "pythoncore-3.11-64"].map((d) => join(local, "Python", d, "python.exe"))
    : [];
  return candidates.find((p) => existsSync(p) && p.toLowerCase() !== discovered?.toLowerCase()) ?? discovered;
}

export default async function settings({ launchApp, artifactsDir, assert }) {
  const shots = [];
  const shot = async (page, name) => {
    // Notifications from earlier steps would cover the view (not while a modal is open).
    if ((await page.locator('[role="dialog"], [role="alertdialog"]').count()) === 0) {
      const dismiss = page.getByRole("button", { name: /^Dismiss notification/ });
      for (let n = await dismiss.count(); n > 0; n--) await dismiss.first().click();
      await dismiss.first().waitFor({ state: "detached" });
    }
    await settle(page);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };
  const evidence = {};
  const base = await mkdtemp(join(tmpdir(), "xp-settings-"));
  const dataDir = join(base, "data");
  const project = join(base, "paper");
  const marker = join(project, "exposition-dev-ran.txt");
  await mkdir(dataDir, { recursive: true });
  await mkdir(project, { recursive: true });
  await writeFile(join(project, "main.typ"), "= Paper\n");
  const env = { EXPOSITION_TOOL_CACHE_DIR: join(artifactsDir, "..", "tool-cache") };
  let pythonOverride;

  // ---------------- First session ----------------
  let app = await launchApp({ dataDir, env });
  try {
    const { page } = app;
    await prepare(page);

    // --- Open Settings from the palette; Mod+, opens it too.
    await runCommand(page, assert, "open settings", /Preferences: Open Settings/);
    const view = page.getByTestId("settings-view");
    await view.waitFor();
    assert.equal(await page.getByRole("tab", { name: "Appearance" }).getAttribute("aria-selected"), "true");
    await page.keyboard.press("Control+w");
    await view.waitFor({ state: "detached" });
    await page.keyboard.press("Control+Comma");
    await view.waitFor();

    // --- Theme.
    await setTheme(page, assert, "Dark", "dark");
    await shot(page, "settings-appearance-dark");
    await setTheme(page, assert, "Light", "light");
    await shot(page, "settings-appearance-light");

    // --- Trust a project through the normal prompt (a refused job offers it).
    const refusedId = await invoke(page, "dev_start_workspace_job", { root: project });
    const refused = await waitFor("refusal", async () =>
      (await invoke(page, "job_list")).find((j) => j.info.id === refusedId && j.info.result),
    );
    assert.equal(refused.refusal?.code, "untrustedWorkspace");
    assert.ok(!existsSync(marker), "an untrusted project never executes");
    const notice = page.getByTestId("notification").filter({ hasText: "Workspace not trusted" });
    await notice.getByRole("button", { name: "Trust workspace…" }).click();
    const trustDialog = page.getByRole("alertdialog", { name: "Trust this workspace?" });
    await trustDialog.waitFor();
    await trustDialog.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await trustDialog.waitFor({ state: "detached" });
    const trusted = await invoke(page, "trust_status", { root: project });
    assert.equal(trusted.trusted, true);
    const canonical = trusted.root;
    evidence.trustedRoot = canonical;

    // --- It is listed under Trusted folders with its grant time.
    await section(page, "Trusted folders");
    const row = page.getByTestId("trusted-folder").filter({ hasText: canonical });
    await row.waitFor();
    assert.match(await row.innerText(), /Trusted .*\d{4}/, "shows when it was trusted");
    const listed = (await invoke(page, "trust_list")).roots.find((r) => r.path === canonical);
    assert.ok(listed?.grantedAtMs > Date.now() - 10 * 60_000, `grant time recorded: ${JSON.stringify(listed)}`);
    assert.ok(
      (await page.getByTestId("trust-rules").innerText()).includes("project folder"),
      "shows the too-broad rule text",
    );
    assert.equal(await page.getByTestId("settings-trust").locator("input, textarea").count(), 0, "no free-form trust input");
    await shot(page, "settings-trust-light");

    // --- Revoke (confirm; the safe choice is focused), then a job is refused again.
    await row.getByRole("button", { name: `Revoke trust for ${canonical}`, exact: true }).click();
    const revokeDialog = page.getByRole("alertdialog", { name: "Revoke trust for this folder?" });
    await revokeDialog.waitFor();
    assert.ok(
      await revokeDialog.getByRole("button", { name: "Cancel" }).evaluate((el) => el === document.activeElement),
      "Cancel is focused",
    );
    await page.waitForTimeout(300);
    await shot(page, "settings-trust-revoke-light");
    await revokeDialog.getByRole("button", { name: "Revoke trust", exact: true }).click();
    await revokeDialog.waitFor({ state: "detached" });
    await row.waitFor({ state: "detached" });
    assert.equal((await invoke(page, "trust_status", { root: project })).trusted, false);
    const againId = await invoke(page, "dev_start_workspace_job", { root: project });
    const again = await waitFor("second refusal", async () =>
      (await invoke(page, "job_list")).find((j) => j.info.id === againId && j.info.result),
    );
    assert.equal(again.refusal?.code, "untrustedWorkspace", "revoked trust refuses native jobs again");
    assert.equal(again.info.startedAtMs, null);
    assert.ok(!existsSync(marker), "nothing ran after revocation");
    evidence.refusalAfterRevoke = again.refusal;

    // --- Restricted execution: Docker status and a LOCAL image.
    await section(page, "Execution");
    const dockerStatus = page.getByTestId("docker-status");
    await dockerStatus.getByText("Available").waitFor({ timeout: 180_000 }); // first discovery may unpack tools
    evidence.dockerStatus = (await dockerStatus.innerText()).replace(/\s+/g, " ");
    const images = await invoke(page, "docker_images_list");
    assert.ok(images.some((i) => i.reference === IMAGE), `${IMAGE} must be present locally: ${JSON.stringify(images)}`);
    evidence.localImages = images.map((i) => i.reference);
    const imageRow = page.getByTestId("docker-image");
    const imageButton = imageRow.locator(".ui-select__button");
    await waitFor("image list loaded", async () => !(await imageButton.isDisabled()));
    await imageButton.click();
    await page.getByRole("option", { name: new RegExp(`^${IMAGE.replace(/[.]/g, "\\.")}`) }).click();
    await waitForAsync(page, async (image) => (await window.__TAURI_INTERNALS__.invoke("execution_settings_get")).settings.docker.image === image, IMAGE, {
      message: "the execution service to report the chosen image",
    });
    await page.getByTestId("docker-image-state").filter({ hasText: `${IMAGE} is present locally` }).waitFor();
    assert.ok((await imageButton.innerText()).includes(IMAGE));
    assert.match(await page.getByTestId("restricted-guarantees").innerText(), /--network=none/);
    await shot(page, "settings-execution-light");

    // --- Server-side validation of the IPC commands.
    const badImage = await rejection(page, "execution_settings_update", {
      change: { kind: "dockerImage", image: "python:3.13-slim; calc" },
    });
    assert.equal(badImage?.code, "settings.invalidImage");
    const optionImage = await rejection(page, "execution_settings_update", {
      change: { kind: "dockerImage", image: "--privileged" },
    });
    assert.equal(optionImage?.code, "settings.invalidImage");
    const rootUser = await rejection(page, "execution_settings_update", { change: { kind: "dockerUser", user: "0:0" } });
    assert.ok(rootUser, "container user/limits are not changeable over IPC");
    const component = await rejection(page, "execution_settings_update", {
      change: { kind: "toolPath", tool: "python-jupyter", path: "C:\\x.exe" },
    });
    assert.equal(component?.code, "settings.unknownTool");
    const state = await invoke(page, "execution_settings_get");
    assert.equal(state.settings.docker.image, IMAGE, "refused changes leave the image unchanged");
    assert.equal(state.settings.docker.user, "65534:65534");
    evidence.ipcRefusals = { badImage, optionImage, rootUser: String(rootUser?.message ?? rootUser), component };

    // --- Tool locations: invalid paths are refused with clear messages.
    const tools = page.getByTestId("tool-locations");
    const pythonRow = tools.locator('tr[data-tool-id="python"]');
    await waitFor("python discovered", async () => (await pythonRow.getAttribute("data-status")) === "available", 60_000);
    const discovered = (await invoke(page, "toolchains_list")).find((t) => t.id === "python");
    pythonOverride = alternatePython(discovered.path);
    evidence.pythonBefore = { path: discovered.path, version: discovered.version, source: discovered.source };

    await tools.getByRole("button", { name: "Set location of Python" }).click();
    const dialog = page.getByRole("dialog", { name: "Location of Python" });
    await dialog.waitFor();
    const input = dialog.getByLabel("Executable path");
    const save = dialog.getByRole("button", { name: "Use this executable" });

    const missing = join(base, "no-such-python.exe");
    await input.fill(missing);
    await save.click();
    const missingError = dialog.getByText(/does not exist/);
    await missingError.waitFor();
    evidence.missingRefusal = await missingError.innerText();

    const script = join(base, "python.cmd");
    await writeFile(script, "@echo off\r\necho hi\r\n");
    await input.fill(script);
    await save.click();
    const cmdError = dialog.getByText(/cmd\.exe/);
    await cmdError.waitFor();
    evidence.cmdRefusal = await cmdError.innerText();
    await shot(page, "settings-tool-refused-light");
    assert.deepEqual((await invoke(page, "execution_settings_get")).settings.toolPaths, {}, "refused paths are not stored");

    // --- A real interpreter is accepted and discovery reports it.
    await input.fill(pythonOverride);
    await save.click();
    await dialog.waitFor({ state: "detached" });
    const after = await waitForAsync(
      page,
      async (expected) => {
        const python = (await window.__TAURI_INTERNALS__.invoke("toolchains_list")).find((t) => t.id === "python");
        return python?.source === "override" && python.path === expected && python.status === "available" ? python : null;
      },
      pythonOverride,
      { timeout: 120_000, message: "discovery to report the python override" },
    );
    evidence.pythonAfter = { path: after.path, version: after.version, source: after.source };
    await waitFor("override row", async () => (await pythonRow.getAttribute("data-source")) === "override", 60_000);
    assert.equal(await pythonRow.getByTestId("tool-path").innerText(), pythonOverride);
    await pythonRow.getByRole("button", { name: "Use the discovered Python" }).waitFor();
    await pythonRow.scrollIntoViewIfNeeded();
    await shot(page, "settings-tools-light");

    // --- Agents: the existing provider dialog, never the key.
    await section(page, "Agents");
    await page.getByTestId("agents-key").waitFor();
    await page.getByTestId("open-provider-settings").click();
    const providerDialog = page.getByRole("dialog", { name: "Agent provider" });
    await providerDialog.waitFor();
    await providerDialog.getByRole("button", { name: "Close" }).click();
    await providerDialog.waitFor({ state: "detached" });
    // The provider dialog lives in the Agent view, which it revealed; hide it again.
    await page.keyboard.press("Control+Alt+b");
    await page.locator('[data-region="auxiliary"]').waitFor({ state: "detached" });

    // Dark screenshots of this session's sections; the dark choice persists.
    await section(page, "Appearance");
    await setTheme(page, assert, "Dark", "dark");
    await section(page, "Execution");
    await page.getByTestId("docker-image-state").filter({ hasText: `${IMAGE} is present locally` }).waitFor();
    await shot(page, "settings-execution-dark");
    await page.getByTestId("tool-locations").locator('tr[data-tool-id="python"]').scrollIntoViewIfNeeded();
    await shot(page, "settings-tools-dark");
    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await closeGracefully(app);
  }

  // ---------------- Relaunch on the same data directory ----------------
  app = await launchApp({ dataDir, env });
  try {
    const { page } = app;
    await prepare(page);
    assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dark", "theme persisted");
    const state = await invoke(page, "execution_settings_get");
    assert.equal(state.settings.docker.image, IMAGE, "image persisted across relaunch");
    assert.equal(state.settings.toolPaths.python, pythonOverride, "tool override persisted across relaunch");
    evidence.afterRelaunch = state.settings;

    await page.keyboard.press("Control+Comma");
    await page.getByTestId("settings-view").waitFor();
    await section(page, "Execution");
    const imageButton = page.getByTestId("docker-image").locator(".ui-select__button");
    await waitFor("image shown", async () => (await imageButton.innerText()).includes(IMAGE));
    const python = await waitForAsync(
      page,
      async () => (await window.__TAURI_INTERNALS__.invoke("toolchains_list")).find((t) => t.id === "python"),
      null,
      { timeout: 120_000 },
    );
    assert.equal(python.source, "override");
    assert.equal(python.path, pythonOverride);

    // --- About: capabilities from service status.
    await section(page, "About");
    const capabilities = page.getByTestId("capabilities");
    const capability = (id) => capabilities.locator(`tr[data-capability="${id}"]`);
    await waitFor(
      "capabilities checked",
      async () => (await capabilities.locator('tr[data-state="checking"]').count()) === 0,
      120_000,
    );
    assert.equal(await capability("typst").getAttribute("data-state"), "available");
    assert.match(await capability("typst").innerText(), /typst 0\.\d+/);
    assert.equal(await capability("restricted").getAttribute("data-state"), "available");
    assert.match(await capability("restricted").innerText(), new RegExp(IMAGE.replace(/[.]/g, "\\.")));
    assert.equal(await capability("pdflatex").getAttribute("data-state"), "available");
    assert.equal(await capability("research-python").getAttribute("data-state"), "available");
    evidence.capabilities = await capabilities.locator("tbody tr").evaluateAll((rows) =>
      rows.map((r) => ({ id: r.dataset.capability, state: r.dataset.state, text: r.innerText.replace(/\s+/g, " ") })),
    );
    assert.ok((await page.getByTestId("about-data-dir").innerText()).includes(dataDir.split(/[\\/]/).pop()));
    await shot(page, "settings-about-dark");

    for (const [name, file] of [
      ["Trusted folders", "settings-trust"],
      ["Research", "settings-research"],
      ["Agents", "settings-agents"],
      ["Editor", "settings-editor"],
    ]) {
      await section(page, name);
      if (name === "Research") await page.getByTestId("research-environments").waitFor({ timeout: 120_000 });
      if (name === "Agents") await page.getByTestId("agents-key").waitFor();
      await shot(page, `${file}-dark`);
    }
    // Editor preferences go through the workspace settings service.
    await section(page, "Editor");
    const minimap = page.getByRole("switch", { name: "Minimap" });
    const initial = (await invoke(page, "settings_get")).settings.editor.minimap;
    await minimap.focus();
    await page.keyboard.press("Space");
    await waitForAsync(page, async (v) => (await window.__TAURI_INTERNALS__.invoke("settings_get")).settings.editor.minimap === v, !initial, {
      message: "minimap setting saved",
    });
    await page.keyboard.press("Space");
    await waitForAsync(page, async (v) => (await window.__TAURI_INTERNALS__.invoke("settings_get")).settings.editor.minimap === v, initial, {
      message: "minimap setting restored",
    });

    await section(page, "Appearance");
    await setTheme(page, assert, "Light", "light");
    for (const [name, file] of [
      ["Editor", "settings-editor"],
      ["Agents", "settings-agents"],
      ["Research", "settings-research"],
      ["About", "settings-about"],
    ]) {
      await section(page, name);
      if (name === "Research") await page.getByTestId("research-environments").waitFor({ timeout: 120_000 });
      if (name === "Agents") await page.getByTestId("agents-key").waitFor();
      await shot(page, `${file}-light`);
    }
    assert.deepEqual(consoleErrors(app), [], "no console errors after relaunch");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "settings");
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${(await readdir(target)).length} screenshots to ${target}`);
  }
}
