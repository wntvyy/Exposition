import { afterEach, describe, expect, it } from "vitest";

import { parseThemePreference, resolveTheme, setThemePreference, THEME_STORAGE_KEY, toggleTheme, useTheme } from "./theme";

describe("theme", () => {
  afterEach(() => {
    localStorage.clear();
    setThemePreference("system");
  });

  it("follows the OS unless the user chose a theme", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
  });

  it("falls back to following the OS for missing or invalid stored values", () => {
    expect(parseThemePreference(null)).toBe("system");
    expect(parseThemePreference("purple")).toBe("system");
    expect(parseThemePreference('"dark"')).toBe("dark");
    expect(parseThemePreference("{not json")).toBe("system");
  });

  it("applies and persists an explicit choice", () => {
    setThemePreference("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe('"dark"');
    expect(useTheme.getState()).toMatchObject({ preference: "dark", resolved: "dark" });
  });

  it("reads every pre-V2 value unchanged and accepts the four built-in themes", () => {
    for (const legacy of ["light", "dark", "system"] as const) expect(parseThemePreference(JSON.stringify(legacy))).toBe(legacy);
    expect(parseThemePreference('"ocean"')).toBe("ocean");
    expect(parseThemePreference('"sage"')).toBe("sage");
    expect(parseThemePreference('"Ocean"')).toBe("system");
    expect(parseThemePreference("42")).toBe("system");
  });

  it("applies Ocean and Sage by name and keeps them across a reload of the stored value", () => {
    setThemePreference("ocean");
    expect(document.documentElement.dataset.theme).toBe("ocean");
    expect(parseThemePreference(localStorage.getItem(THEME_STORAGE_KEY))).toBe("ocean");
    expect(resolveTheme("sage", true)).toBe("sage");
    setThemePreference("sage");
    expect(useTheme.getState()).toMatchObject({ preference: "sage", resolved: "sage" });
  });

  it("toggles by kind from Ocean and Sage", () => {
    setThemePreference("ocean");
    toggleTheme();
    expect(useTheme.getState().resolved).toBe("light");
    setThemePreference("sage");
    toggleTheme();
    expect(useTheme.getState().resolved).toBe("dark");
  });

  it("toggles between light and dark from the resolved theme", () => {
    setThemePreference("light");
    toggleTheme();
    expect(useTheme.getState().resolved).toBe("dark");
    toggleTheme();
    expect(useTheme.getState().resolved).toBe("light");
  });
});
