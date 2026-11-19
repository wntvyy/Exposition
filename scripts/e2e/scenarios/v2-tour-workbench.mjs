// V2 tour of the workbench surfaces (UI standard V2, phase 4), in all four
// themes, with populated content:
// - shell: page bar, Editor-only activity rail, Files tree with long names,
//   folders (one named like a file, "legacy.py"), and the supplied file icons;
//   editor tabs (an unsaved tab, long names), Split view of a Typst file with
//   a build error, the Problems panel grouped by file, status bar;
// - keyboard focus and selection in the Files tree; the command palette;
// - the New File dialog with the backend's refusal under the field;
// - a persistent notification over the workbench;
// - Settings: Appearance, Editor, Model providers with an OpenAI model-list
//   error (a saved fake key against a closed loopback endpoint; debug builds
//   only, isolated credential namespace), About;
// - the narrow window (sidebar auto-collapsed; Settings in its narrow layout).
// Shots: test-results/e2e/v2-tour-workbench/ and, with EXPOSITION_DEV_HOME,
// $EXPOSITION_DEV_HOME/ui-reference/v2/workbench/<name>-<theme>.png.
import { copyFile, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { runCommand } from "../palette.mjs";
import { THEMES } from "../theme-helpers.mjs";
import { invoke, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const AREA = "workbench";
const WIDE = { width: 1440, height: 900 };
const NARROW = { width: 820, height: 760 };
const FAKE_KEY = "sk-e2e-v2-tour-not-a-real-key-0000000000";
const LONG_TEX = "sections/appendix_derivation_of_the_modified_arrhenius_equation_with_tunnelling_corrections.tex";
const LONG_R = "analysis/bootstrap_confidence_intervals_for_the_activation_energy.R";

const MAIN = `#set page(width: 14cm, height: 18cm, margin: 1.5cm)
#set par(justify: true)

= Results <sec:results>

The corrected calibration of 2026-03-14 gives an activation energy of
$E_a = 53.4 plus.minus 0.6 "kJ/mol"$, consistent with the bound proved in
the formal appendix.

#let R = 8.314
#let A = 1.2e7

For $T_1 < T_2$ the fitted rate satisfies $k(T_1) < k(T_2)$.
`;

const FILES = {
  "main.typ": MAIN,
  "sections/results.typ": "= Results\n\nSee the main document.\n",
  "sections/methods.md": "# Methods\n\nRates were measured at six temperatures.\n",
  [LONG_TEX]: "\\section{Derivation}\nThe modified Arrhenius equation $k = A T^n e^{-E_a/RT}$.\n",
  "analysis/arrhenius_fit.py": "import numpy as np\n\n\ndef fit(T, k):\n    return np.polyfit(1 / T, np.log(k), 1)\n",
  [LONG_R]: "boot <- function(x) mean(sample(x, replace = TRUE))\n",
  "analysis/kinetics_model.jl": "k(T; A=1.2e7, Ea=53.4e3) = A * exp(-Ea / (8.314 * T))\n",
  "analysis/exploratory_rate_constants.ipynb": JSON.stringify({ cells: [], metadata: {}, nbformat: 4, nbformat_minor: 5 }),
  "analysis/legacy.py/old_fit.py": "# superseded\n",
  "data/rates.csv": "T,k\n300,0.012\n320,0.041\n",
  "data/measurements.json": '{ "temperatures": [300, 320] }\n',
  "figures/arrhenius.svg": '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>\n',
  "proofs/ArrheniusBounds.lean": "theorem rate_monotone : True := trivial\n",
  "references.bib": "@article{arrhenius1889, title={On the reaction velocity}, year={1889}}\n",
  "pyproject.toml": '[project]\nname = "arrhenius-kinetics"\n',
  "README.md": "# Arrhenius kinetics\n",
  "notes.unknownext": "plain notes\n",
};

async function closedLoopbackUrl() {
  // A port that was free a moment ago: requests to it are refused quickly.
  const port = await new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port: p } = server.address();
      server.close(() => resolve(p));
    });
  });
  // Exactly http://127.0.0.1:<port> (debug builds ignore any other form and
  // would reach the real provider).
  return `http://127.0.0.1:${port}`;
}

/**
 * Switch theme through its "Color Theme: …" command. (theme-helpers'
 * applyTheme anchors its match at the start of the palette row, which begins
 * with the "Preferences:" category, so it never matches.)
 */
async function applyTheme(page, theme) {
  const label = THEMES.find(([id]) => id === theme)?.[1];
  await runCommand(page, `Color Theme: ${label}`, new RegExp(`Color Theme: ${label}`));
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
  await page.waitForTimeout(150);
}

async function setViewport(page, size) {
  await page.setViewportSize(size);
  await page.waitForFunction((w) => window.innerWidth === w, size.width);
  await page.waitForTimeout(200);
}

const regionVisible = async (page, name) => (await page.locator(`[data-region="${name}"]`).count()) > 0;

async function expandFolder(page, path) {
  const row = page.locator(`[data-testid="file-tree"] [data-path="${path}"]`).first();
  await row.waitFor({ timeout: 10_000 });
  if ((await row.getAttribute("aria-expanded")) !== "true") await row.locator(".ui-tree-item__chevron").first().click();
  await page.waitForFunction((p) => document.querySelector(`[data-testid="file-tree"] [data-path="${p}"]`)?.getAttribute("aria-expanded") === "true", path);
}

async function openFile(page, path) {
  await page.locator(`[data-testid="file-tree"] [data-path="${path}"] .ui-tree-item__title`).first().click();
  await page.getByRole("tab", { name: new RegExp(path.split("/").pop().replace(/[.*+?^${}()|[\]\\]/g, "\\$&")) }).first().waitFor({ timeout: 10_000 });
}

export default async function v2TourWorkbench({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-v2-tour-"));
  const root = join(base, "arrhenius-kinetics");
  for (const [path, text] of Object.entries(FILES)) {
    await mkdir(dirname(join(root, path)), { recursive: true });
    await writeFile(join(root, path), text);
  }
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-v2-tour-${Date.now()}`, EXPOSITION_DEV_OPENAI_BASE_URL: await closedLoopbackUrl() };
  const app = await launchApp({ env });
  const { page } = app;
  const published = process.env.EXPOSITION_DEV_HOME ? join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "v2", AREA) : null;
  if (published) await mkdir(published, { recursive: true });
  const written = [];

  /**
   * One shot per theme. `prepare` re-creates transient state after the theme
   * switch (the palette takes focus); `cleanup` closes it before the next one.
   */
  async function shoot(name, { prepare, cleanup } = {}) {
    for (const [theme] of THEMES) {
      await applyTheme(page, theme);
      if (prepare) await prepare(theme);
      await page.waitForTimeout(150);
      const path = join(artifactsDir, `${name}-${theme}.png`);
      await page.screenshot({ path });
      if (published) await copyFile(path, join(published, `${name}-${theme}.png`));
      written.push(path);
      if (cleanup) await cleanup(theme);
    }
  }

  try {
    const info = await invoke(page, "app_info");
    assert.ok(info.debugBuild, "the tour saves a fake provider key: debug builds only (isolated credential namespace)");
    await setViewport(page, WIDE);
    await page.getByTestId("workbench").waitFor({ timeout: 15_000 });
    await openWorkspace(page, root);
    // The auxiliary panel belongs to the Agents tour; hide it here.
    if (await regionVisible(page, "auxiliary")) await page.keyboard.press("Control+Alt+b");

    // --- Files tree: long names, folder named like a file, supplied icons.
    for (const folder of ["analysis", "data", "figures", "proofs", "sections"]) await expandFolder(page, folder);
    assert.equal(
      await page.locator('[data-testid="file-tree"] [data-path="analysis/legacy.py"]').getAttribute("data-kind"),
      "directory",
      "a folder named like a file stays a folder",
    );
    const assetIcons = await page.locator('[data-testid="file-tree"] [data-file-icon]').count();
    assert.ok(assetIcons >= 8, `the tree uses the supplied file icons (${assetIcons})`);

    // --- Tabs: long names, then the Typst file in Split with an unsaved error.
    await openFile(page, LONG_R);
    await openFile(page, "analysis/arrhenius_fit.py");
    await openFile(page, "main.typ");
    const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
    await modeButton("Split").waitFor({ timeout: 10_000 });
    await modeButton("Split").click();
    await page.locator('[data-testid="page"] img').first().waitFor({ timeout: 30_000 });
    const doc = (await invoke(page, "document_list")).find((d) => d.path === "main.typ").id;
    const state = await invoke(page, "document_state", { document: doc });
    const at = state.text.indexOf("For $T_1");
    await invoke(page, "document_apply", {
      request: {
        id: "v2-tour-error",
        origin: { kind: "source" },
        edits: [{ document: doc, baseRevision: state.info.revision, patches: [{ range: { start: at, end: at }, text: "#let k(T) = A * calc.exp(-Ea_kj / (R * T))\nAt 300 K the rate is #k(300).\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await page.getByRole("tab", { name: "Problems" }).click();
    await page.locator('[data-testid="problem"]').first().waitFor({ timeout: 30_000 });
    assert.match(await page.locator('[data-testid="problem"]').first().textContent(), /Ea_kj/, "the build error is listed");
    assert.equal(await page.locator('.problems__group[data-path="main.typ"]').count(), 1, "problems are grouped under their file");
    await shoot("editor-problems");

    // --- Keyboard focus and multi-selection in the Files tree.
    const focusTree = async () => {
      await page.locator('[data-testid="file-tree"] [data-path="analysis/arrhenius_fit.py"] .ui-tree-item__title').first().click({ modifiers: ["Control"] });
      await page.keyboard.press("ArrowDown");
    };
    await shoot("files-focus", { prepare: focusTree });

    // --- Command palette.
    await shoot("palette", {
      prepare: async () => {
        await page.keyboard.press("Control+Shift+P");
        await page.locator(".wb-palette__input").fill("toggle");
        await page.locator(".wb-palette__item").first().waitFor();
      },
      cleanup: async () => {
        await page.keyboard.press("Escape");
        await page.locator(".wb-palette__input").waitFor({ state: "detached" });
      },
    });

    // --- New File dialog with the refusal under the field (name taken).
    await shoot("dialog-new-file", {
      prepare: async () => {
        await page.locator('[data-testid="file-tree"] [data-path="README.md"] .ui-tree-item__title').first().click();
        await page.keyboard.press("Control+n");
        const dialog = page.getByTestId("file-name-dialog");
        await dialog.waitFor();
        await dialog.locator("input").fill("main.typ");
        await page.getByTestId("file-name-submit").click();
        await dialog.getByText(/exists|already/i).first().waitFor({ timeout: 10_000 });
      },
      cleanup: async () => {
        await page.getByTestId("file-name-cancel").click();
        await page.getByTestId("file-name-dialog").waitFor({ state: "detached" });
      },
    });

    // --- A persistent notification (the shared host), over the workbench.
    await runCommand(page, "ui reference", /Developer: Show UI Reference/);
    await page.getByTestId("ui-reference").waitFor({ timeout: 10_000 });
    await page.getByRole("button", { name: "Danger", exact: true }).click();
    await page.getByRole("region", { name: "Notifications" }).waitFor();
    await page.getByRole("tab", { name: /main\.typ/ }).first().click();
    await shoot("notification");
    const dismiss = page.getByRole("button", { name: /^Dismiss notification/ });
    while (await dismiss.count()) await dismiss.first().click();
    await page.getByRole("tab", { name: "UI Reference" }).first().click();
    await page.keyboard.press("Control+w");
    await page.getByTestId("ui-reference").waitFor({ state: "detached" });

    // --- Settings: Appearance, Editor, About.
    const section = async (name) => {
      await page.getByRole("tab", { name, exact: true }).click();
      await page.waitForTimeout(100);
    };
    await runCommand(page, "open settings", /Preferences: Open Settings/);
    await page.getByTestId("settings-view").waitFor();
    await section("Appearance");
    await shoot("settings-appearance");
    await section("Editor");
    await page.getByTestId("settings-editor-prefs").getByText("Font size").waitFor();
    await shoot("settings-editor");
    await section("About");
    await page.getByTestId("about-version").waitFor();
    await shoot("settings-about");

    // --- Model providers: OpenAI key saved, its model list fails (closed endpoint).
    await section("Model providers");
    const providers = page.getByTestId("settings-providers");
    await providers.waitFor();
    const openai = providers.getByTestId("settings-provider-openai");
    await openai.getByTestId("agents-key-input").locator("input").fill(FAKE_KEY);
    await openai.getByTestId("agents-key-save").click();
    await waitFor(async () => /saved/.test((await openai.getByTestId("agents-settings-message").textContent().catch(() => "")) ?? ""), {
      message: "OpenAI key saved",
    });
    await waitFor(async () => /Could not load|did not list/.test((await openai.getByTestId("settings-provider-catalog").textContent()) ?? ""), {
      timeout: 30_000,
      message: "the model list error",
    });
    assert.equal(await page.evaluate((k) => document.documentElement.outerHTML.includes(k), FAKE_KEY), false, "the key is not in the webview");
    assert.doesNotMatch(
      (await openai.getByTestId("settings-provider-catalog").textContent()) ?? "",
      /openai\.com/,
      "the model list error comes from the closed loopback endpoint, not the real provider",
    );
    await shoot("settings-providers-error", {
      prepare: async () => {
        await providers.locator(".exp-settings__body").evaluate((el) => el.scrollTo(0, 0));
      },
    });
    await shoot("settings-providers-defaults", {
      prepare: async () => {
        await providers.getByTestId("agents-default").scrollIntoViewIfNeeded();
      },
    });
    // Leave no saved key behind in the namespace.
    await invoke(page, "agents_provider_clear_key", { provider: "openai" }).catch(() => {});

    // --- Narrow window: the sidebar collapses; Settings switches to its narrow layout.
    await setViewport(page, NARROW);
    assert.equal(await regionVisible(page, "sidebar"), false, "the sidebar auto-collapses in a narrow window");
    await section("Editor");
    await shoot("narrow-settings");
    await page.getByRole("tab", { name: /main\.typ/ }).first().click();
    await shoot("narrow-editor");
    await setViewport(page, WIDE);

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
    console.log(`v2-tour-workbench: ${written.length} shots${published ? ` (copied to ${published})` : ""}`);
  } finally {
    await app.close();
  }
}
