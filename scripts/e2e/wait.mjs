// Polling helpers for e2e scenarios.
//
// Playwright's `page.waitForFunction` does not await an async predicate: the
// returned Promise is truthy, so the wait succeeds at once. Any condition that
// needs `await` in the page (e.g. an IPC call) must use `waitForAsync`, which
// polls from Node and awaits each evaluation.

/**
 * Poll `fn(arg)` in the page until it returns a truthy value; returns that value.
 * @param {import("playwright-core").Page} page
 * @param {Function} fn evaluated in the page; may be async
 * @param {unknown} [arg]
 * @param {{ timeout?: number, interval?: number, message?: string }} [options]
 */
export async function waitForAsync(page, fn, arg, options = {}) {
  const timeout = options.timeout ?? 10_000;
  const interval = options.interval ?? 50;
  const deadline = Date.now() + timeout;
  let lastError;
  for (;;) {
    try {
      const value = await page.evaluate(fn, arg);
      if (value) return value;
      lastError = undefined;
    } catch (error) {
      lastError = error;
    }
    if (Date.now() >= deadline) {
      const what = options.message ?? String(fn).replace(/\s+/g, " ").slice(0, 160);
      const cause = lastError ? ` (last error: ${lastError.message ?? lastError})` : "";
      throw new Error(`timed out after ${timeout} ms waiting for ${what}${cause}`);
    }
    await new Promise((resolve) => setTimeout(resolve, interval));
  }
}
