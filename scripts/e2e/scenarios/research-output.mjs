// Regression for the R-m4 research output findings (#8, #10, #12) in the real
// app: an output flood (~23 MB written by the kernel) keeps the UI responsive
// (rAF frame gaps measured in the page while the Research panel shows the run),
// a 20 KB HTML table with huge colspan/rowspan values cannot expand into a
// multi-megabyte grid, and research sessions are capped per owner with an
// explicit refusal, leaving interactive-job slots for other services.
//
// Observations: test-results/e2e/research-output/observations.json. Set
// EXPOSITION_E2E_EXECUTABLE to run against another build.
import { cp, mkdir, mkdtemp, readdir, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";
import { runCommand } from "../palette.mjs";

const KERNEL_TIMEOUT = 120_000;
/** Largest acceptable main-thread stall while output streams in. */
const FRAME_BUDGET_MS = 200;

const submit = (page, sessionId, code) =>
  invoke(page, "research_execute", { request: { sessionId, source: { kind: "input", code }, requester: { kind: "user" } } });

async function settle(page, runId, timeout = KERNEL_TIMEOUT) {
  return waitFor(
    async () => {
      const m = await invoke(page, "research_run", { runId });
      return m.record.status.kind !== "running" ? m : null;
    },
    { timeout, interval: 200, message: `run ${runId} to finish` },
  );
}

export default async function researchOutput({ launchApp, artifactsDir, fixture, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-research-out-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("research-python"), root, { recursive: true });
  await mkdir(dataDir, { recursive: true });

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };

  const app = await launchApp({ dataDir });
  try {
    const { page } = app;
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });
    const ws = await invoke(page, "research_workspace");
    await invoke(page, "trust_grant", { root: ws.root });
    await runCommand(page, "Show Research", /Show Research/);
    await page.getByTestId("research-panel").waitFor({ timeout: 15_000 });
    const report = await invoke(page, "research_environments", { refresh: true });
    const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    assert.ok(python, "Python available");
    const session = await invoke(page, "research_session_start", {
      request: { environmentId: python.id, owner: { kind: "user" } },
    });

    // ---- #8 output flood: the page keeps painting --------------------------
    await page.evaluate(() => {
      const gaps = { max: 0, last: performance.now(), frames: 0, stop: false };
      window.__xpGaps = gaps;
      const tick = (t) => {
        gaps.max = Math.max(gaps.max, t - gaps.last);
        gaps.last = t;
        gaps.frames += 1;
        if (!gaps.stop) requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
    const flood = await submit(
      page,
      session.id,
      'import sys\nfor i in range(400):\n    sys.stdout.write(("\\x1b[31m" + "x" * 50_000 + "\\x1b[0m"))\nprint("y" * 3_000_000)\nprint("done")',
    );
    const floodManifest = await settle(page, flood.id);
    await page
      .locator(`[data-testid="research-output"][data-run-id="${flood.id}"][data-status="succeeded"]`)
      .waitFor({ timeout: 60_000 });
    await page.waitForTimeout(500);
    const gaps = await page.evaluate(() => {
      window.__xpGaps.stop = true;
      return { max: Math.round(window.__xpGaps.max), frames: window.__xpGaps.frames };
    });
    const nodes = await page.getByTestId("research-stream").count();
    note(
      "output-flood-keeps-ui-responsive",
      gaps.max < FRAME_BUDGET_MS && floodManifest.output.droppedStreamChars > 0,
      `worst frame gap ${gaps.max} ms over ${gaps.frames} frames; kept ${floodManifest.output.streamChars} chars, dropped ${floodManifest.output.droppedStreamChars}; ${nodes} stream nodes`,
    );
    await page.screenshot({ path: join(artifactsDir, "after-flood.png") });

    // ---- #10 table-span amplification --------------------------------------
    const amplify = await submit(
      page,
      session.id,
      'from IPython.display import HTML, display\ndisplay(HTML("<table><tr>" + "<td colspan=1000 rowspan=1000>x</td>" * 25 + "</tr>" + "<tr><td>y</td></tr>" * 999 + "</table>"))\nprint("after table")',
    );
    const started = Date.now();
    const ampManifest = await settle(page, amplify.id, 300_000);
    const seconds = (Date.now() - started) / 1000;
    let csvBytes = 0;
    const dir = join(root, ".exposition", "artifacts", amplify.id);
    for (const f of await readdir(dir).catch(() => [])) if (f.endsWith(".csv")) csvBytes += (await stat(join(dir, f))).size;
    note(
      "table-span-amplification-bounded",
      ampManifest.record.status.kind === "succeeded" && csvBytes < 1_000_000 && seconds < 15,
      `${(csvBytes / 1e3).toFixed(0)} KB CSV, ${seconds.toFixed(1)} s, status ${ampManifest.record.status.kind}`,
    );

    // ---- #12 per-owner session cap ---------------------------------------
    const extra = [];
    let refusal = null;
    for (let i = 0; i < 20; i++) {
      const r = await invokeError(page, "research_session_start", {
        request: { environmentId: python.id, owner: { kind: "user" } },
      });
      if (r.error) {
        refusal = r.error;
        break;
      }
      extra.push(r.unexpectedSuccess.id);
    }
    const live = (await invoke(page, "research_sessions")).filter((s) => !["stopped", "detached"].includes(s.state.kind));
    note(
      "research-sessions-capped-per-owner",
      refusal?.code === "research.tooManySessions" && live.length <= 8,
      `${live.length} live sessions; next refused with ${JSON.stringify(refusal)}`,
    );
    for (const id of [...extra, session.id]) await invoke(page, "research_session_shutdown", { sessionId: id }).catch(() => {});
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
