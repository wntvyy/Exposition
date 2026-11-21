// Startup: time from spawning the app to the workbench being rendered, and
// the memory and page counters of the idle app with no project open.
export default async function startup(ctx) {
  const runs = 5;
  const samples = [];
  let idle = null;
  for (let i = 0; i < runs; i++) {
    const started = performance.now();
    const app = await ctx.launchApp();
    try {
      await app.page.waitForSelector("#root > *", { timeout: 30_000 });
      samples.push(performance.now() - started);
      if (i === runs - 1) {
        await ctx.sleep(5_000);
        idle = await ctx.metrics(app);
      }
    } finally {
      await app.close();
    }
  }
  // The first launch pays for creating the WebView2 profile; report it apart.
  ctx.record({ bench: "app.startup.first_launch", samples_ms: samples.slice(0, 1) });
  ctx.record({ bench: "app.startup.launch", samples_ms: samples.slice(1) });
  if (idle) ctx.record({ bench: "app.startup.idle_no_project", counters: idle });
}
