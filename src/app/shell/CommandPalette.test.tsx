// Command palette keyboard behavior: Enter runs the highlighted command, else
// the top match for the text in the field — however that text got there.
//
// react-aria's Menu runs only an item holding its virtual focus, and moves that
// focus to the first match only after forward typing (a `beforeinput` of type
// insertText). After Backspace, a paste, or an edit it saw no typing event for,
// the list showed the match with nothing highlighted and Enter did nothing: the
// palette stayed open (the e2e "palette did not close after Enter" flake).
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { registry } from "../registry";
import { CommandPalette, commandPalette } from "./CommandPalette";

const ran: string[] = [];
const offs: Array<() => void> = [];
const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;

function register(id: string, title: string) {
  offs.push(registry.registerCommand({ id, title, category: "Test", run: () => void ran.push(id) }));
}

/** One edit as the browser delivers it: beforeinput (unless omitted), new value, input. */
function edit(input: HTMLInputElement, value: string, inputType: string | null) {
  if (inputType) {
    input.dispatchEvent(new InputEvent("beforeinput", { inputType, bubbles: true, cancelable: true }));
  }
  setValue.call(input, value);
  input.dispatchEvent(new InputEvent("input", { inputType: inputType ?? undefined, bubbles: true }));
}

function typeText(input: HTMLInputElement, text: string) {
  for (const ch of text) edit(input, input.value + ch, "insertText");
}

function press(input: HTMLInputElement, key: string) {
  input.dispatchEvent(new KeyboardEvent("keydown", { key, code: key, bubbles: true, cancelable: true }));
  input.dispatchEvent(new KeyboardEvent("keyup", { key, code: key, bubbles: true, cancelable: true }));
}

/** Let React's scheduled work and the palette's deferred command run. */
const settle = () => new Promise((r) => setTimeout(r, 50));

async function openPalette(): Promise<HTMLInputElement> {
  render(<CommandPalette />);
  act(() => commandPalette.open());
  const input = (await screen.findByRole("textbox", { name: "Command" })) as HTMLInputElement;
  // Real browser timing: native events, React flushing on its own schedule
  // rather than `act` flushing after every event.
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  return input;
}

const idByLabel: Record<string, string> = {
  "Test: Alpha Action": "test.alpha",
  "Test: Beta Action": "test.beta",
  "Test: Gamma Action": "test.gamma",
};

/** The listed commands in order, and whether react-aria highlights each (virtual focus). */
const items = () =>
  [...document.querySelectorAll<HTMLElement>(".wb-palette__item")].map((e) => ({
    id: idByLabel[e.textContent ?? ""] ?? e.textContent,
    focused: e.hasAttribute("data-focused"),
  }));

describe("CommandPalette Enter", () => {
  beforeEach(() => {
    ran.length = 0;
    register("test.alpha", "Alpha Action");
    register("test.beta", "Beta Action");
    register("test.gamma", "Gamma Action");
  });

  afterEach(() => {
    act(() => commandPalette.close());
    cleanup();
    while (offs.length) offs.pop()!();
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  });

  it("runs the top match after a typo is corrected with Backspace", async () => {
    const input = await openPalette();
    typeText(input, "gamx");
    await settle();
    edit(input, "gam", "deleteContentBackward");
    await settle();
    // The list shows the match, but react-aria has highlighted nothing.
    expect(items()).toEqual([{ id: "test.gamma", focused: false }]);
    press(input, "Enter");
    await settle();
    expect(ran).toEqual(["test.gamma"]);
    expect(commandPalette.isOpen()).toBe(false);
  });

  it("runs the top match for a pasted query", async () => {
    const input = await openPalette();
    edit(input, "beta", "insertFromPaste");
    await settle();
    press(input, "Enter");
    await settle();
    expect(ran).toEqual(["test.beta"]);
    expect(commandPalette.isOpen()).toBe(false);
  });

  it("runs the top match when the field changed without a typing event", async () => {
    const input = await openPalette();
    edit(input, "alpha", null);
    await settle();
    press(input, "Enter");
    await settle();
    expect(ran).toEqual(["test.alpha"]);
  });

  it("runs the top match for the text in the field even before the list re-renders", async () => {
    const input = await openPalette();
    typeText(input, "beta");
    // Enter in the same task as the last keystroke.
    press(input, "Enter");
    await settle();
    expect(ran).toEqual(["test.beta"]);
  });

  it("runs the command once when react-aria also highlighted it", async () => {
    const input = await openPalette();
    typeText(input, "gamma");
    await settle();
    expect(items()).toEqual([{ id: "test.gamma", focused: true }]);
    press(input, "Enter");
    await settle();
    expect(ran).toEqual(["test.gamma"]);
  });

  it("runs the item chosen with the arrow keys, not the top match", async () => {
    const input = await openPalette();
    typeText(input, "action");
    await settle();
    press(input, "ArrowDown");
    await settle();
    const chosen = items().find((i) => i.focused);
    expect(chosen?.id).toBeDefined();
    expect(chosen?.id).not.toBe(items()[0]!.id);
    press(input, "Enter");
    await settle();
    expect(ran).toEqual([chosen!.id]);
  });

  it("does nothing when nothing matches", async () => {
    const input = await openPalette();
    typeText(input, "zzzz");
    await settle();
    press(input, "Enter");
    await settle();
    expect(ran).toEqual([]);
    expect(commandPalette.isOpen()).toBe(true);
  });

  it("does nothing on Enter with an empty query and nothing highlighted", async () => {
    const input = await openPalette();
    await settle();
    press(input, "Enter");
    await settle();
    expect(ran).toEqual([]);
    expect(commandPalette.isOpen()).toBe(true);
  });

  it("marks the list as filtered so the top match shows as the default", async () => {
    const input = await openPalette();
    const list = () => document.querySelector(".wb-palette__list")!;
    expect(list().hasAttribute("data-query")).toBe(false);
    typeText(input, "gam");
    await settle();
    expect(list().hasAttribute("data-query")).toBe(true);
    // The default-highlight rule targets the first item as a direct child.
    expect(list().firstElementChild?.getAttribute("data-command")).toBe("test.gamma");
  });
});
