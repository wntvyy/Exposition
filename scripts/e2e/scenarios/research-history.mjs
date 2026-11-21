// Run history with more stored runs than one page (real app, no kernel: the
// runs are records written into a disposable project, as an earlier session
// would have left them).
//  1. 1,130 run records and one damaged record on the second page: the
//     history shows its first 200 rows and "200 of 1,131 records" beside the
//     pane title. "Show more runs" adds rows, reading the next page when the
//     loaded runs are used up, until every run is listed. The total stays
//     1,131 when the damaged record is found; it is then named in its own
//     row ("cannot be read"), and the count ends at "1,130 of 1,131 records".
//  2. `research_runs` returns the first page (500 runs), the total (1,131
//     record files), and a continuation; three pages hold every run once and
//     count the damaged record apart; a foreign continuation is refused.
//  3. The oldest run (last row of the last page) is selected and its stored
//     output shown.
//  4. It is deleted with the Delete key and brought back with Undo; every
//     page stays loaded and the selection stays where it was.
//  5. A run of the last page is deleted from its row menu while another run
//     stays selected.
//  6. Storage & History lists every run record, not the first page; the
//     deleted record is restored there and is listed in the history again.
// Screens: test-results/e2e/research-history/.
import { mkdir, mkdtemp, utimes, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { consoleErrors, dismissNotifications, expectNoDialog, pressNotificationAction, settle, waitForNotification } from "../p1-helpers.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const RUNS = 1_130;
const PAGE = 500;
const ROWS = 200;
/** Position (by age) of the damaged record: read with the second page. */
const DAMAGED = 400;
const T0 = Date.UTC(2026, 0, 5, 9, 0, 0);

const runId = (i) => `run-${i.toString(16).padStart(16, "0")}`;

function manifest(i) {
  const code = `print("run number ${i}")\n`;
  return {
    format: "exposition.run/1",
    record: {
      id: runId(i),
      producer: { kind: "kernelSession", sessionId: "session-earlier", runtime: "python" },
      inputs: [],
      environment: [{ name: "python", version: "3.14.0" }],
      outputs: [],
      status: { kind: "succeeded" },
      startedAtMs: T0 + i * 60_000,
      finishedAtMs: T0 + i * 60_000 + 1_500,
    },
    session: {
      sessionId: "session-earlier",
      owner: { kind: "user" },
      environmentId: "python:earlier",
      generation: 1,
      interpreter: { implementation: "CPython", version: "3.14.0", location: "~" },
      kernel: { name: "python3", language: "python", languageVersion: "3.14.0", implementation: "ipython", implementationVersion: "9.0" },
      helperVersion: "1",
      protocol: 1,
    },
    code: { origin: { kind: "input" }, sha256: "0".repeat(64), bytes: code.length, text: code },
    executionCount: i + 1,
    error: null,
    output: { entries: 1, streamChars: 0, droppedStreamChars: 0, droppedFiles: 0, truncated: false },
    unsavedInputs: [],
  };
}

/** Write the records oldest first; each record's modification time follows its run. */
async function writeRuns(root) {
  const runs = join(root, ".exposition", "runs");
  await mkdir(runs, { recursive: true });
  for (let i = 0; i < RUNS; i += 1) {
    const file = join(runs, `${runId(i)}.json`);
    await writeFile(file, JSON.stringify(manifest(i), null, 2));
    const at = new Date(T0 + i * 60_000 + 2_000);
    await utimes(file, at, at);
    const artifacts = join(root, ".exposition", "artifacts", runId(i));
    await mkdir(artifacts, { recursive: true });
    await writeFile(
      join(artifacts, "transcript.json"),
      JSON.stringify([{ seq: 1, item: { kind: "stream", name: "stdout", text: `run number ${i}\n` } }]),
    );
  }
  // A record that is not a manifest, between two runs of the second page.
  const damaged = join(runs, "run-damaged-record.json");
  await writeFile(damaged, "{ this record was cut short");
  const at = new Date(T0 + DAMAGED * 60_000 + 2_500);
  await utimes(damaged, at, at);
}

export default async function researchHistory({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-research-history-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "main.typ"), "= Notes\n");
  await writeRuns(root);
  const evidence = { notes: [] };
  const app = await launchApp({ dataDir });
  const { page } = app;
  const shot = async (name) => {
    await settle(page);
    await page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  };

  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });

    // --- 1. the history list: rows on demand ------------------------------------------------
    await runCommand(page, "Show Research Panel", /Show Research Panel/);
    const history = page.getByTestId("research-history");
    await history.waitFor({ timeout: 15_000 });
    const rows = history.locator("[data-op-item]");
    const row = (i) => history.locator(`[data-op-item="${runId(i)}"]`);
    // The count beside the pane title (the shared PanelHeader's count).
    const headerCount = page.locator(".exp-research__history .ui-panel-header__count");
    const countText = () => headerCount.innerText();
    const more = page.getByTestId("research-history-more");
    /** The stream output shown for run `i` once it is the run shown. */
    const shownOutput = async (i) => {
      const stream = page.locator(`[data-testid="research-output"][data-run-id="${runId(i)}"] [data-testid="research-stream"]`);
      await stream.first().waitFor({ timeout: 15_000 });
      return stream.first().textContent();
    };
    await waitFor(async () => (await rows.count()) === ROWS, { timeout: 15_000, message: "the first 200 rows" });
    // 1,131 record files; the damaged one is on a page not read yet.
    assert.equal(await countText(), "200 of 1,131 records");
    assert.equal(await row(0).count(), 0, "the oldest run is not listed yet");
    assert.match(await more.innerText(), /^Show more runs\s+931 records not shown$/);
    await shot("research-history-first-rows");

    const counts = [];
    for (let shown = 2 * ROWS; shown < RUNS; shown += ROWS) {
      await more.click();
      await waitFor(async () => (await rows.count()) === shown, { timeout: 20_000, message: `${shown} rows` });
      counts.push(await countText());
      if (shown === 3 * ROWS) {
        // 600 rows: the second page was read for them.
        await more.scrollIntoViewIfNeeded();
        await shot("research-history-second-page");
      }
    }
    assert.deepEqual(
      counts,
      ["400 of 1,131 records", "600 of 1,131 records", "800 of 1,131 records", "1,000 of 1,131 records"],
      "the total stays what it was when the damaged record is found (with the second page)",
    );
    assert.match(await more.innerText(), /^Show more runs\s+130 records not shown$/);
    await more.click();
    await waitFor(async () => (await rows.count()) === RUNS, { timeout: 20_000, message: "every run listed" });
    assert.equal(await countText(), "1,130 of 1,131 records", "listed and unreadable add up to the total");
    const inset = await page.evaluate(() => {
      const header = document.querySelector(".exp-research__history .ui-panel-header");
      const title = header.querySelector(".ui-panel-header__title").getBoundingClientRect();
      const count = header.querySelector(".ui-panel-header__count").getBoundingClientRect();
      const box = header.getBoundingClientRect();
      return { title: Math.round(title.left - box.left), gap: Math.round(count.left - title.right), right: Math.round(box.right - count.right) };
    });
    assert.equal(inset.title, 14, "the title starts at the pane inset");
    assert.ok(inset.gap >= 4 && inset.gap <= 12, `the count follows the title (${inset.gap}px)`);
    evidence.notes.push(`header count placement: ${JSON.stringify(inset)}`);
    assert.equal(await more.count(), 0, "no further page");
    assert.match(await page.getByTestId("research-history-unreadable").innerText(), /^1 run record cannot be read\s+Not listed as runs$/);
    const listed = await rows.evaluateAll((items) => items.map((item) => item.getAttribute("data-op-item")));
    assert.deepEqual(listed, Array.from({ length: RUNS }, (_, i) => runId(RUNS - 1 - i)), "every run once, newest first");
    evidence.notes.push(`history rows: ${ROWS} → ${counts.join(" → ")} → ${RUNS} (all); unreadable note shown`);

    // --- 2. the command: first page, total, continuation ---------------------------------
    const first = await invoke(page, "research_runs");
    assert.equal(first.runs.length, PAGE, "the first page holds 500 runs");
    assert.equal(first.total, RUNS + 1, "every record file is counted");
    assert.equal(first.hasMore, true);
    assert.equal(first.runs[0].id, runId(RUNS - 1), "newest first");
    const second = await invoke(page, "research_runs", { request: { after: first.next } });
    assert.equal(second.runs.length, PAGE);
    assert.equal(second.unreadable, 1, "the damaged record is counted apart");
    const third = await invoke(page, "research_runs", { request: { after: second.next } });
    assert.equal(third.runs.length, RUNS - 2 * PAGE);
    assert.equal(third.hasMore, false);
    assert.equal(third.next, null);
    assert.equal(third.runs.at(-1).id, runId(0), "the last page ends with the oldest run");
    const paged = [...first.runs, ...second.runs, ...third.runs].map((r) => r.id);
    assert.equal(new Set(paged).size, RUNS, "every run once across the pages");
    const refused = await invoke(page, "research_runs", { request: { after: "h1.0.0.0.run-0" } }).then(
      () => null,
      (e) => e,
    );
    assert.ok(refused, "a continuation of another listing is refused");
    evidence.notes.push(`research_runs: pages of ${first.runs.length}/${second.runs.length}/${third.runs.length}, total ${first.total}, unreadable ${third.unreadable}; foreign continuation: ${JSON.stringify(refused)}`);

    // --- 3. the oldest run: selected, output shown ---------------------------------------------
    await row(0).scrollIntoViewIfNeeded();
    await row(0).click();
    assert.equal(await shownOutput(0), "run number 0\n", "the stored output of the oldest run is shown");
    await waitFor(async () => (await row(0).getAttribute("data-selected")) !== null, { timeout: 5_000, message: "the oldest run is the selected row" });
    await shot("research-history-last-page-selected");

    // --- 4. Delete key and Undo on a run of the last page ----------------------------------------
    await row(0).focus();
    await page.keyboard.press("Delete");
    await expectNoDialog(page, assert, 600, "a finished run record is deleted at once");
    await row(0).waitFor({ state: "detached", timeout: 15_000 });
    assert.equal(await rows.count(), RUNS - 1, "the other pages stay loaded after the delete");
    const deleted = await waitForNotification(page, /./);
    await pressNotificationAction(page, deleted.title, "Undo");
    await row(0).waitFor({ timeout: 15_000 });
    assert.equal(await rows.count(), RUNS, "restored into the loaded list");
    assert.equal((await rows.last().getAttribute("data-op-item")) ?? "", runId(0), "back at its place (the oldest)");
    evidence.notes.push(`Delete key on the oldest run: "${deleted.title}"; Undo restored it; ${RUNS} rows before and after`);
    await dismissNotifications(page);

    // --- 5. row menu delete, restore from Storage & History ----------------------------------------
    await row(700).scrollIntoViewIfNeeded();
    await row(700).click();
    assert.equal(await shownOutput(700), "run number 700\n");
    await row(3).scrollIntoViewIfNeeded();
    await shot("research-history-all-rows");
    await row(3).click({ button: "right" });
    const menu = page.getByTestId("research-runs-context-menu");
    await menu.waitFor({ timeout: 5_000 });
    await menu.getByRole("menuitem", { name: /^Delete Run Record/ }).click();
    await row(3).waitFor({ state: "detached", timeout: 15_000 });
    assert.equal(await rows.count(), RUNS - 1, "a run of the last page deleted from its row menu");
    assert.equal(await row(700).getAttribute("data-selected"), "true", "the selection stays on the run shown");
    await dismissNotifications(page);

    // --- 6. Storage & History lists every record ------------------------------------------------
    await runCommand(page, "Storage & History", /Preferences: Storage & History/);
    await page.getByTestId("settings-storage").waitFor({ timeout: 15_000 });
    const records = page.locator('[data-testid="storage-item"][data-key^="researchRuns|"]');
    await waitFor(async () => (await records.count()) > 0, { timeout: 30_000, message: "storage lists research run records" });
    assert.equal(await records.count(), RUNS - 1, "Storage & History lists every run record, not the first page");
    assert.equal(await page.locator(`[data-testid="storage-item"][data-key="researchRuns|researchRun:${runId(0)}"]`).count(), 1, "the oldest record is listed");
    evidence.notes.push(`Storage & History: ${await records.count()} research run records listed`);
    const recoverable = page.locator(`[data-testid="storage-item"][data-key="recoverableDeletions|recoverable:researchRun:${runId(3)}"]`);
    await recoverable.waitFor({ timeout: 15_000 });
    await recoverable.getByTestId("storage-restore").click();
    await waitFor(async () => (await invoke(page, "research_run", { runId: runId(3) }).then(() => true, () => false)), { timeout: 15_000, message: "run 3 restored" });
    // Back in the history: every page is still loaded and the run is listed again.
    await runCommand(page, "Show Research Panel", /Show Research Panel/);
    await history.waitFor({ timeout: 15_000 });
    for (let i = 0; i < 8 && (await row(3).count()) === 0 && (await more.count()) > 0; i += 1) {
      const before = await rows.count();
      await more.click();
      await waitFor(async () => (await rows.count()) > before, { timeout: 15_000, message: "more rows" });
    }
    await row(3).waitFor({ timeout: 15_000 });
    await waitFor(async () => (await rows.count()) === RUNS, { timeout: 15_000, message: "every run listed again after the restore" });
    evidence.notes.push("row menu delete of a last-page run, restored from Storage & History, listed again with every page loaded");

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
}
