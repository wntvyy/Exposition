// Command palette helper shared by e2e scenarios.
//
// Runs a command the way a user does: Ctrl+Shift+P, type the query, Enter.
// It checks that the palette's top match is the expected command before
// pressing Enter, and that Enter closes the palette. It does not retry: a
// palette that stays open after Enter is a product failure, reported with the
// palette's state and recent focus/input events.

const DIAG_KEY = "__xpPaletteDiag";

async function installDiagnostics(page) {
  await page.evaluate((key) => {
    if (window[key]) return;
    const log = [];
    window[key] = log;
    const describe = (node) => {
      if (!node || !node.getAttribute) return node === window ? "window" : String(node?.nodeName ?? node);
      return (node.getAttribute("aria-label") || node.className || node.nodeName || "").toString().slice(0, 40);
    };
    const record = (event) => {
      const inPalette = event.target?.closest?.(".wb-palette");
      if (!inPalette && !["focusin", "focusout"].includes(event.type)) return;
      log.push(
        `${Math.round(performance.now())} ${event.isTrusted ? "T" : "v"} ${event.type}` +
          `${event.inputType ? `(${event.inputType})` : ""}${event.key ? `[${event.key}]` : ""} ${describe(event.target)}` +
          `${event.relatedTarget ? ` <- ${describe(event.relatedTarget)}` : ""}`,
      );
      if (log.length > 200) log.splice(0, log.length - 200);
    };
    for (const type of ["focusin", "focusout", "beforeinput", "input", "keydown", "pointerover"]) {
      document.addEventListener(type, record, true);
    }
  }, DIAG_KEY);
}

async function paletteState(page) {
  return page.evaluate((key) => {
    const input = document.querySelector(".wb-palette__input");
    return {
      value: input?.value ?? null,
      activeElement: document.activeElement?.getAttribute("aria-label") ?? document.activeElement?.nodeName ?? null,
      activeDescendant: input?.getAttribute("aria-activedescendant") ?? null,
      items: [...document.querySelectorAll(".wb-palette__item")].slice(0, 5).map((item) => ({
        text: item.textContent,
        focused: item.hasAttribute("data-focused"),
      })),
      events: (window[key] ?? []).slice(-40),
    };
  }, DIAG_KEY);
}

/**
 * Run a command through the command palette.
 * @param {import("playwright-core").Page} page
 * @param {string} query text typed into the palette
 * @param {RegExp} [expected] the top match's label must match (default: contains the query, case-insensitively)
 */
export async function runCommand(page, query, expected) {
  await installDiagnostics(page);
  await page.evaluate((key) => window[key].push(`---- runCommand`), DIAG_KEY);
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByRole("textbox", { name: "Command" });
  await input.waitFor({ state: "visible", timeout: 5000 });
  await input.fill(query);
  const matches = expected ?? new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "i");
  // The filtered list must rank the command first before Enter is meaningful.
  const deadline = Date.now() + 5000;
  for (;;) {
    const top = await page.locator(".wb-palette__item").first().textContent({ timeout: 5000 }).catch(() => null);
    if (top !== null && matches.test(top)) break;
    if (Date.now() > deadline) {
      throw new Error(`palette top match for "${query}" was "${top}": ${JSON.stringify(await paletteState(page))}`);
    }
    await page.waitForTimeout(25);
  }
  await page.keyboard.press("Enter");
  const closed = await input.waitFor({ state: "detached", timeout: 5000 }).then(
    () => true,
    () => false,
  );
  if (!closed) {
    const state = await paletteState(page);
    throw new Error(`the command palette stayed open after Enter for "${query}": ${JSON.stringify(state, null, 1)}`);
  }
}
