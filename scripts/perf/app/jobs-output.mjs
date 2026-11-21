// Job output in the real app: while jobs print (the debug build's probe jobs:
// a burst of 400 lines, then a stdout and a stderr line every 0.5 s each),
// count the `jobs:event` events delivered to the webview and their bytes,
// the `job_list` and `job_output` commands the page issues and the bytes
// they return, and how responsive the page stays (long tasks, worst frame
// gap). Measured with the Jobs panel hidden, then with it open and one job
// selected. Needs the debug build (`dev_start_probe_job`) and Python.
const JOBS = { small: 2, large: 6, stress: 12 };
const WINDOW_MS = { small: 10_000, large: 20_000, stress: 30_000 };

const invoke = (page, cmd, args) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);

/** Count commands by name (calls, response bytes), job events, long tasks and frame gaps. */
async function instrument(page) {
  await page.evaluate(async () => {
    const internals = window.__TAURI_INTERNALS__;
    const perf = (window.__perf = { calls: {}, bytes: {}, events: 0, eventBytes: 0, outputEvents: 0, outputTextBytes: 0, longTasks: 0, longTaskMs: 0, maxFrameGapMs: 0, frames: 0 });
    const original = internals.invoke.bind(internals);
    // Commands travel as requests to the IPC origin (`…/<command>`); the
    // invoke function itself is read-only, so the requests are counted.
    const nativeFetch = window.fetch.bind(window);
    window.fetch = (input, init) => {
      const url = typeof input === "string" ? input : (input?.url ?? String(input));
      const result = nativeFetch(input, init);
      if (/ipc/.test(url)) {
        perf.ipcRequests = (perf.ipcRequests ?? 0) + 1;
        const cmd = url.split("/").pop();
        if (cmd === "job_list" || cmd === "job_output") {
          perf.calls[cmd] = (perf.calls[cmd] ?? 0) + 1;
          result.then(
            (response) =>
              response
                .clone()
                .arrayBuffer()
                .then((body) => {
                  perf.bytes[cmd] = (perf.bytes[cmd] ?? 0) + body.byteLength;
                }),
            () => {},
          );
        }
      }
      return result;
    };
    const handler = internals.transformCallback((event) => {
      perf.events += 1;
      perf.eventBytes += JSON.stringify(event.payload).length;
      if (event.payload?.kind === "output") {
        perf.outputEvents += 1;
        perf.outputTextBytes += event.payload.text?.length ?? 0;
      }
    });
    await original("plugin:event|listen", { event: "jobs:event", target: { kind: "Any" }, handler });
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        perf.longTasks += 1;
        perf.longTaskMs += entry.duration;
      }
    }).observe({ entryTypes: ["longtask"] });
    let last = performance.now();
    const frame = (now) => {
      perf.frames += 1;
      perf.maxFrameGapMs = Math.max(perf.maxFrameGapMs, now - last);
      last = now;
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
  });
}

const reset = (page) =>
  page.evaluate(() => {
    const p = window.__perf;
    p.ipcRequests = 0;
    Object.assign(p, { calls: {}, bytes: {}, events: 0, eventBytes: 0, outputEvents: 0, outputTextBytes: 0, longTasks: 0, longTaskMs: 0, maxFrameGapMs: 0, frames: 0 });
  });

const read = (page) => page.evaluate(() => JSON.parse(JSON.stringify(window.__perf)));

function counters(sample, windowMs, extra) {
  const perSecond = (n) => Number(((n ?? 0) / (windowMs / 1000)).toFixed(2));
  return {
    // Every command request the page issued in the window (any command).
    ipc_requests: sample.ipcRequests ?? 0,
    job_events: sample.events,
    job_event_bytes: sample.eventBytes,
    output_events: sample.outputEvents,
    output_text_bytes_in_events: sample.outputTextBytes,
    job_list_calls: sample.calls.job_list ?? 0,
    job_list_calls_per_second: perSecond(sample.calls.job_list),
    job_list_response_bytes: sample.bytes.job_list ?? 0,
    job_output_calls: sample.calls.job_output ?? 0,
    job_output_response_bytes: sample.bytes.job_output ?? 0,
    ipc_bytes_per_second: perSecond(sample.eventBytes + (sample.bytes.job_list ?? 0) + (sample.bytes.job_output ?? 0)),
    long_tasks: sample.longTasks,
    long_task_ms: Math.round(sample.longTaskMs),
    max_frame_gap_ms: Math.round(sample.maxFrameGapMs),
    frames: sample.frames,
    ...extra,
  };
}

export default async function jobsOutput(ctx) {
  const jobs = JOBS[ctx.scale] ?? JOBS.small;
  const windowMs = WINDOW_MS[ctx.scale] ?? WINDOW_MS.small;
  const app = await ctx.launchApp();
  try {
    const { page } = app;
    await page.waitForSelector("#root > *", { timeout: 30_000 });
    await instrument(page);
    const ids = [];
    for (let i = 0; i < jobs; i++) ids.push(await invoke(page, "dev_start_probe_job"));
    // Let the bursts pass; measure the steady stream.
    await ctx.sleep(3_000);
    const retained = (await invoke(page, "job_list")).length;
    const scale = { printing_jobs: jobs, retained_jobs: retained, window_ms: windowMs };

    await reset(page);
    await ctx.sleep(windowMs);
    const hidden = await read(page);
    ctx.record({ bench: "app.jobs.output_stream.panel_hidden", scale, counters: counters(hidden, windowMs, await ctx.metrics(app, { collect: false })) });

    // Open the Jobs panel and select one printing job.
    await page.evaluate(() =>
      localStorage.setItem(
        "exposition.workbench.layout",
        JSON.stringify({
          version: 1,
          sidebar: { visible: true, size: 220, activeView: null },
          panel: { visible: true, size: 560, activeView: "jobs.panel" },
          auxiliary: { visible: false, size: 320, activeView: null },
          editorSplit: 50,
        }),
      ),
    );
    await page.reload();
    await page.waitForFunction(() => (document.getElementById("root")?.childElementCount ?? 0) > 0);
    await instrument(page);
    await page.getByRole("tab", { name: "Jobs" }).click();
    const panel = page.getByTestId("jobs-panel");
    await panel.waitFor();
    const row = panel.locator(`li.exp-jobs__row[data-job-id="${ids[0]}"]`);
    await row.locator(".exp-jobs__select").click();
    await page.getByTestId("job-output").waitFor();
    await ctx.sleep(1_000);
    await reset(page);
    await ctx.sleep(windowMs);
    const open = await read(page);
    ctx.record({
      bench: "app.jobs.output_stream.panel_open",
      scale,
      counters: counters(open, windowMs, await ctx.metrics(app, { collect: false })),
    });
    for (const id of ids) await invoke(page, "job_cancel", { job: id });
    await ctx.sleep(1_500);
    const left = (await invoke(page, "job_list")).filter((j) => j.info.result === null && j.kind !== "probe").length;
    ctx.record({ bench: "app.jobs.output_stream.after_cancel", scale, counters: { jobs_still_running: left } });
  } finally {
    await app.close();
  }
}
