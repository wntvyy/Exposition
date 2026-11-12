// R-m4 independent review: Lean status honesty and inert rendering in the
// real app's Lean panel (UI, not only IPC).
//
// 1. A file whose only theorem proves False through a `sorry` hidden by
//    `#guard_msgs` is shown as "Checked" (DEFECT), next to a control file
//    with a visible sorry that is shown as "Incomplete (sorry)".
// 2. Goals and hovers containing HTML-looking text render as text: no script
//    runs, no element is injected, no network request is made.
//
// Requires leanprover/lean4:v4.35.0 through elan. Observations go to
// test-results/e2e/review-m4-lean-ui/observations.json; "DEFECT" observations
// fail the scenario at the end.
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { alertDialog, editorLocator, invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";

async function runCommand(page, query, expectedTitle) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByRole("textbox", { name: "Command" });
  await input.waitFor({ state: "visible", timeout: 5000 });
  await input.fill(query);
  const first = page.locator(".wb-palette__item").first();
  await first.waitFor({ state: "visible" });
  const title = await first.innerText();
  if (!expectedTitle.test(title)) throw new Error(`palette top match for "${query}" was "${title}"`);
  await page.keyboard.press("Enter");
  await input.waitFor({ state: "detached", timeout: 5000 });
}

export default async function reviewLeanUi({ launchApp, artifactsDir, fixture, assert }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", "leanprover--lean4---v4.35.0", "bin"))) {
    console.log("SKIPPED: Lean toolchain leanprover/lean4:v4.35.0 is not installed through elan");
    return;
  }
  const hits = [];
  const server = createServer((req, res) => {
    hits.push(req.url);
    res.end();
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const beacon = `http://127.0.0.1:${server.address().port}`;

  const base = await mkdtemp(join(tmpdir(), "xp-rm4-lean-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("lean-basic"), root, { recursive: true, filter: (src) => !/[\\/]\.lake([\\/]|$)/.test(src) });
  await mkdir(dataDir, { recursive: true });
  await writeFile(
    join(root, "LeanBasic", "Hidden.lean"),
    "/-- warning: declaration uses `sorry` -/\n#guard_msgs in\ntheorem hidden : False := sorry\n",
  );
  await writeFile(join(root, "LeanBasic", "Visible.lean"), "theorem visible : False := sorry\n");
  await writeFile(
    join(root, "LeanBasic", "Markup.lean"),
    `/-- <img src="${beacon}/hover-img" onerror="document.title='pwned-hover'"><script>document.title='pwned-hover'</script> **bold** [run](command:workbench.action.quit) -/\ndef markupDoc : Nat := 1\n\ntheorem markupGoal (h : "<img src='${beacon}/goal-img' onerror=alert(1)><b>bold</b>" = "a") : markupDoc = 1 := by\n  skip\n  rfl\n`,
  );

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok, detail });
    console.log(`${ok ? "ok    " : "DEFECT"} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };

  const app = await launchApp({ dataDir });
  const { page } = app;
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="lakefile.toml"]').waitFor({ timeout: 15_000 });
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    const panel = page.getByTestId("lean-panel");
    const status = () => panel.getAttribute("data-status");

    await openInEditor(page, "LeanBasic/Visible.lean");
    await waitFor(async () => (await status()) === "untrusted", { timeout: 20_000, message: "untrusted" });
    await panel.getByRole("button", { name: "Trust workspace…" }).click();
    await alertDialog(page).getByRole("button", { name: "Trust workspace", exact: true }).click();
    await waitFor(async () => ["checkedWithSorry", "checked", "errors"].includes(await status()), { timeout: 300_000, interval: 250, message: "Visible.lean finished" });
    const visibleText = await page.getByTestId("lean-status").innerText();
    note("visible-sorry-shown-incomplete", (await status()) === "checkedWithSorry", visibleText);

    await openInEditor(page, "LeanBasic/Hidden.lean");
    await waitFor(async () => /Hidden\.lean/.test(await panel.innerText()) && ["checkedWithSorry", "checked", "errors"].includes(await status()), {
      timeout: 120_000,
      interval: 250,
      message: "Hidden.lean finished",
    });
    const hiddenText = await page.getByTestId("lean-status").innerText();
    await page.screenshot({ path: join(artifactsDir, "hidden-sorry-shown-checked.png") });
    note("DEFECT-ui-shows-checked-for-hidden-sorry", (await status()) !== "checked", `Lean panel for a proof of False via #guard_msgs-hidden sorry: "${hiddenText}" (status ${await status()})`);

    // Markup in goals and hovers.
    const markupId = await openInEditor(page, "LeanBasic/Markup.lean");
    await waitFor(async () => /Markup\.lean/.test(await panel.innerText()) && ["checked", "errors", "checkedWithSorry"].includes(await status()), {
      timeout: 120_000,
      interval: 250,
      message: "Markup.lean checked",
    });
    const editor = editorLocator(page, "LeanBasic/Markup.lean");
    await editor.locator(".view-line", { hasText: /^\s*skip\s*$/ }).click();
    const goals = page.getByTestId("lean-goals");
    await waitFor(async () => (await goals.innerText()).includes("<b>bold</b>"), { timeout: 60_000, message: "goal with markup text" });
    const goalImgs = await goals.locator("img, b, script").count();
    // Hover the definition name to show its docstring.
    const nameBox = await editor.locator(".view-line", { hasText: /^def\smarkupDoc/ }).boundingBox();
    await page.mouse.move(nameBox.x + 60, nameBox.y + nameBox.height / 2);
    const hover = page.locator(".monaco-hover-content").first();
    let hoverText = "";
    try {
      await hover.waitFor({ state: "visible", timeout: 20_000 });
      hoverText = await hover.innerText();
    } catch {
      const offset = (await invoke(page, "document_state", { document: markupId })).text.indexOf("markupDoc") + 2;
      const h = await invoke(page, "lean_hover", { document: markupId, revision: (await invoke(page, "document_state", { document: markupId })).info.revision, offset });
      hoverText = `(no Monaco hover shown; lean_hover returned ${JSON.stringify(h).slice(0, 200)})`;
    }
    const hoverInjected = await page.locator(".monaco-hover-content img, .monaco-hover-content script").count();
    // Click the command link if one rendered.
    await page.locator(".monaco-hover-content a").first().click({ timeout: 2000 }).catch(() => {});
    await page.waitForTimeout(1500);
    await page.screenshot({ path: join(artifactsDir, "markup-goal-hover.png") });
    const title = await page.title();
    note(
      "lean-goal-and-hover-markup-inert",
      goalImgs === 0 && hoverInjected === 0 && !/pwned/.test(title) && hits.length === 0 && app.child.exitCode === null,
      `goal elements=${goalImgs}; hover elements=${hoverInjected}; title=${title}; requests=${JSON.stringify(hits)}; hover="${hoverText.slice(0, 160)}"`,
    );

    // A large file with many incomplete declarations: time to a final status
    // and main-thread responsiveness while diagnostics stream in.
    const N = 3000;
    const big = Array.from({ length: N }, (_, i) => `theorem big${i} : ${i} + 0 = ${i} := by\n  sorry\n`).join("\n");
    await writeFile(join(root, "LeanBasic", "Big.lean"), big);
    const t0 = Date.now();
    await openInEditor(page, "LeanBasic/Big.lean");
    let worst = 0;
    let final = null;
    while (Date.now() - t0 < 600_000) {
      const f = Date.now();
      await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => r(1))));
      worst = Math.max(worst, Date.now() - f);
      const inPanel = /Big\.lean/.test(await panel.innerText().catch(() => ""));
      const s = await status();
      if (inPanel && ["checkedWithSorry", "checked", "errors"].includes(s)) {
        final = s;
        break;
      }
      await page.waitForTimeout(250);
    }
    const docs = await invoke(page, "lean_documents");
    const bigState = docs.find((d) => d.path === "LeanBasic/Big.lean");
    note(
      "large-lean-file-status-and-responsiveness",
      final === "checkedWithSorry" && bigState?.status.count === N && worst < 2000,
      `${N} sorry declarations: final=${final} count=${bigState?.status.count} after ${((Date.now() - t0) / 1000).toFixed(1)} s; worst frame ${worst} ms`,
    );
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    server.close();
    await app.close();
  }
  const defects = observations.filter((o) => !o.ok);
  assert.deepEqual(defects.map((d) => d.id), [], `R-m4 defects observed (see ${join(artifactsDir, "observations.json")})`);
}
