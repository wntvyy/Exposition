import { beforeEach, describe, expect, it } from "vitest";

import {
  AUXILIARY_COLLAPSE_WIDTH,
  defaultLayout,
  effectiveVisibility,
  LAYOUT_STORAGE_KEY,
  layout,
  parseLayout,
  resetLayoutForTests,
  useLayout,
} from "./layout";

describe("parseLayout", () => {
  it("returns defaults for missing or malformed data", () => {
    expect(parseLayout(null)).toEqual(defaultLayout);
    expect(parseLayout("{oops")).toEqual(defaultLayout);
    expect(parseLayout("42")).toEqual(defaultLayout);
    expect(parseLayout('{"version":99}')).toEqual(defaultLayout);
  });

  it("keeps valid fields, clamps sizes, and drops invalid ones", () => {
    const raw = JSON.stringify({
      version: 1,
      sidebar: { visible: false, size: 5000, activeView: "files" },
      panel: { visible: true, size: "big", activeView: 7 },
      auxiliary: { visible: "yes", size: 300 },
      editorSplit: 95,
    });
    const parsed = parseLayout(raw);
    expect(parsed.sidebar).toEqual({ visible: false, size: 600, activeView: "files" });
    expect(parsed.panel).toEqual({ ...defaultLayout.panel, visible: true });
    expect(parsed.auxiliary).toEqual({ ...defaultLayout.auxiliary, size: 300 });
    expect(parsed.editorSplit).toBe(80);
  });
});

describe("effectiveVisibility", () => {
  const prefs = { sidebar: true, panel: true, auxiliary: true };
  it("collapses the auxiliary panel, then the sidebar, as the window narrows", () => {
    expect(effectiveVisibility(prefs, 1440, {})).toEqual(prefs);
    expect(effectiveVisibility(prefs, AUXILIARY_COLLAPSE_WIDTH - 1, {})).toEqual({ ...prefs, auxiliary: false });
    expect(effectiveVisibility(prefs, 760, {})).toEqual({ sidebar: false, panel: true, auxiliary: false });
  });

  it("shows a region the user explicitly opened in a narrow window", () => {
    expect(effectiveVisibility(prefs, 760, { sidebar: true })).toEqual({ sidebar: true, panel: true, auxiliary: false });
  });
});

describe("layout store", () => {
  beforeEach(() => {
    localStorage.clear();
    resetLayoutForTests();
  });

  it("toggles visibility and persists it", () => {
    layout.setViewportWidth(1440);
    layout.toggle("sidebar");
    expect(useLayout.getState().sidebar.visible).toBe(false);
    expect(JSON.parse(localStorage.getItem(LAYOUT_STORAGE_KEY)!).sidebar.visible).toBe(false);
    layout.toggle("sidebar");
    expect(useLayout.getState().sidebar.visible).toBe(true);
  });

  it("opening an auto-collapsed region overrides collapse until the window widens", () => {
    layout.setViewportWidth(760);
    expect(layout.isVisible("sidebar")).toBe(false);
    layout.toggle("sidebar");
    expect(layout.isVisible("sidebar")).toBe(true);
    layout.setViewportWidth(1440);
    layout.setViewportWidth(760);
    expect(layout.isVisible("sidebar")).toBe(false);
  });

  it("hiding an auto-collapsed region keeps it hidden after widening", () => {
    layout.setViewportWidth(760);
    layout.toggle("sidebar"); // force open
    layout.toggle("sidebar"); // hide again
    layout.setViewportWidth(1440);
    expect(layout.isVisible("sidebar")).toBe(false);
  });

  it("showView activates a view and reveals its region", () => {
    layout.setViewportWidth(1440);
    layout.setVisible("panel", false);
    layout.showView("panel", "output");
    expect(useLayout.getState().panel).toMatchObject({ visible: true, activeView: "output" });
  });

  it("records sizes within bounds", () => {
    layout.setSize("sidebar", 10);
    expect(useLayout.getState().sidebar.size).toBe(170);
    layout.setEditorSplit(33.3);
    expect(useLayout.getState().editorSplit).toBeCloseTo(33.3);
  });
});
