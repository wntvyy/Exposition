// V2 review samples: behavior of the sample module (render, comparable content
// across directions, the demonstrated interactions, URL round trip). The
// samples are isolated: they set no app theme and touch no stored setting.
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { DIRECTION_IDS } from "./directions";
import { AGENT } from "./mock";
import { DEFAULT_PARAMS, parseSampleParams, sampleSearch, V2Samples, type SampleParams } from "./V2Samples";

afterEach(cleanup);

const show = (p: Partial<SampleParams>) => render(<V2Samples initial={{ ...DEFAULT_PARAMS, ...p }} />);

describe("V2 samples", () => {
  it.each(DIRECTION_IDS)("%s: the Agent view keeps the complete approval payload and the exact decision labels", (sample) => {
    show({ sample, view: "agent" });
    expect(screen.getByTestId("v2-approval-payload").textContent).toBe(AGENT.approval.payload);
    const card = screen.getByTestId("v2-approval");
    expect(within(card).getByText("Nothing is applied until you approve. Review the complete content below.")).toBeTruthy();
    for (const label of ["Approve", "Approve for this task", "Decline"]) expect(within(card).getByRole("button", { name: label })).toBeTruthy();
    expect(within(card).getByText(AGENT.approval.scope)).toBeTruthy();
    expect(screen.getByRole("navigation", { name: "Needs you" })).toBeTruthy();
    expect(screen.getByText(AGENT.options)).toBeTruthy();
  });

  it("selecting a tab shows that file and keeps the unsaved marker on results.typ", () => {
    show({ sample: "compact", view: "editor" });
    expect(screen.getByRole("tab", { name: "results.typ, unsaved changes" }).getAttribute("aria-selected")).toBe("true");
    fireEvent.click(screen.getByRole("tab", { name: "arrhenius_fit.py" }));
    expect(screen.getByRole("tab", { name: "arrhenius_fit.py" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("region", { name: "analysis/arrhenius_fit.py (sample, read-only)" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "results.typ, unsaved changes" })).toBeTruthy();
  });

  it("the Files context menu opens on right-click and closes with Escape", () => {
    show({ sample: "balanced", view: "editor" });
    const row = document.querySelector('[data-path="analysis/arrhenius_fit.py"]')!;
    fireEvent.contextMenu(row);
    const menu = screen.getByTestId("v2-files-menu");
    expect(within(menu).getByText("Moves to the Recycle Bin")).toBeTruthy();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByTestId("v2-files-menu")).toBeNull();
  });

  it("Advanced keeps entered values while collapsed; the key error stays until the field changes", () => {
    show({ sample: "reading", view: "settings" });
    expect(screen.getByTestId("v2-key-error").textContent).toContain("Not saved");
    const toggle = screen.getByTestId("v2-advanced");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(toggle);
    const org = screen.getByLabelText("Organization ID") as HTMLInputElement;
    fireEvent.change(org, { target: { value: "org-lab" } });
    fireEvent.click(toggle);
    fireEvent.click(toggle);
    expect((screen.getByLabelText("Organization ID") as HTMLInputElement).value).toBe("org-lab");
    fireEvent.change(screen.getByLabelText("API key"), { target: { value: "sk-new" } });
    expect(screen.queryByTestId("v2-key-error")).toBeNull();
  });

  it("Appearance offers exactly the four themes without changing the app theme", () => {
    const before = document.documentElement.dataset.theme;
    show({ sample: "balanced", view: "settings", settingsPage: "appearance" });
    const group = screen.getByRole("radiogroup", { name: "Color theme" });
    expect(within(group).getAllByRole("radio")).toHaveLength(4);
    fireEvent.click(within(group).getByLabelText("Ocean"));
    expect(document.documentElement.dataset.theme).toBe(before);
  });

  it("palette sheets and icon samples render with every contrast pair passing", () => {
    for (const palette of ["dark", "light", "ocean", "sage"] as const) {
      show({ sample: "palette", palette });
      const sheet = screen.getByTestId(`v2-sheet-${palette}`);
      expect(sheet.querySelectorAll('tr[data-pass="true"]').length).toBeGreaterThan(100);
      expect(sheet.querySelectorAll('tr[data-pass="false"]')).toHaveLength(0);
      cleanup();
      show({ sample: "icons", palette });
      const tomls = [...document.querySelectorAll('img[data-icon^="toml"]')].map((i) => i.getAttribute("data-icon"));
      expect(new Set(tomls)).toEqual(new Set([palette === "light" || palette === "sage" ? "toml_light" : "toml"]));
      cleanup();
    }
  });

  it("round-trips the URL parameters", () => {
    const p: SampleParams = { ...DEFAULT_PARAMS, sample: "reading", view: "agent", width: 360, chrome: false };
    expect(parseSampleParams(sampleSearch(p))).toEqual(p);
    expect(parseSampleParams("?s=nope&view=bad")).toEqual(DEFAULT_PARAMS);
  });
});
