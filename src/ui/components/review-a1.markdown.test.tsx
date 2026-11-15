// R-a1 (independent review of A1): the renderer boundary for "Run in Research"
// on an agent's code fence. D4 makes the click authorize "that visible
// invocation", and the approval cards show invisible and direction-changing
// characters as ⟨U+XXXX⟩ ("so what the user reads is exactly what runs").
// A code fence's Run action has no such protection.
//
// Tests that demonstrate a CURRENT defect run only with REVIEW_A1_DEFECTS=1
// (they assert the expected behavior and fail on the product):
//   REVIEW_A1_DEFECTS=1 npx vitest run src/ui/components/review-a1.markdown.test.tsx
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({ appOpenExternal: vi.fn() }));
vi.mock("../../lib/ipc", () => ({ commands: ipc }));
vi.mock("./MarkdownHighlight", () => ({
  highlight: vi.fn(async (code: string) => code.split("\n").map((line) => [{ text: line, kind: null }])),
}));

const { Markdown } = await import("./Markdown");

afterEach(() => cleanup());

const defect = process.env.REVIEW_A1_DEFECTS ? it : it.skip;

// A Python line whose comment holds a right-to-left override: everything
// after it on the line is displayed reversed, so the shown line differs from
// the code that runs (Trojan Source, CVE-2021-42574).
const RLO = "‮";
const LRI = "⁦";
const PDI = "⁩";
const CODE = `total = 3  # sum of the sample${RLO} ${LRI}\nprint(total)${PDI}`;

describe("R-a1: Run in Research on a fence with invisible or direction-changing characters", () => {
  it("control: the raw code (with the controls) is what Run receives", () => {
    const run = vi.fn();
    render(<Markdown source={"```python\n" + CODE + "\n```"} runAction={{ label: "Run in Research", run }} />);
    fireEvent.click(screen.getByRole("button", { name: "Run in Research" }));
    expect(run).toHaveBeenCalledWith(CODE, "python");
  });

  defect("R-a1 F5: the fence shows the controls (or refuses Run) so the visible code is what runs", () => {
    const run = vi.fn();
    render(<Markdown source={"```python\n" + CODE + "\n```"} runAction={{ label: "Run in Research", run }} />);
    const shown = screen.getByTestId("md-code-text").textContent ?? "";
    const button = screen.queryByRole("button", { name: "Run in Research" });
    const revealed = /U\+202E/.test(shown) || !shown.includes(RLO);
    const refused = !button || button.hasAttribute("disabled") || button.getAttribute("aria-disabled") === "true";
    expect({ revealed, refused, shownHasRawRLO: shown.includes(RLO) }).toMatchObject(
      revealed || refused ? {} : { revealed: true },
    );
  });
});
