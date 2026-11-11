import { describe, expect, it } from "vitest";

import { fuzzyMatch, rankByFuzzy } from "./fuzzy";

describe("fuzzyMatch", () => {
  it("matches subsequences case-insensitively and reports positions", () => {
    expect(fuzzyMatch("tsb", "Toggle Sidebar")?.positions).toEqual([0, 7, 11]);
    expect(fuzzyMatch("TOGGLE", "toggle panel")).not.toBeNull();
    expect(fuzzyMatch("xyz", "Toggle Sidebar")).toBeNull();
  });

  it("matches everything for an empty query", () => {
    expect(fuzzyMatch("", "anything")).toEqual({ score: 0, positions: [] });
  });

  it("ignores spaces in the query", () => {
    expect(fuzzyMatch("tog side", "View: Toggle Sidebar")).not.toBeNull();
  });
});

describe("rankByFuzzy", () => {
  const items = ["View: Toggle Panel", "View: Toggle Sidebar", "Developer: Show UI Reference", "Preferences: Toggle Theme"];

  it("prefers word-start and contiguous matches", () => {
    expect(rankByFuzzy("sidebar", items, (s) => s)[0]?.item).toBe("View: Toggle Sidebar");
    expect(rankByFuzzy("ui ref", items, (s) => s)[0]?.item).toBe("Developer: Show UI Reference");
    expect(rankByFuzzy("theme", items, (s) => s).map((r) => r.item)).toEqual(["Preferences: Toggle Theme"]);
  });

  it("keeps original order for an empty query", () => {
    expect(rankByFuzzy("", items, (s) => s).map((r) => r.item)).toEqual(items);
  });
});
