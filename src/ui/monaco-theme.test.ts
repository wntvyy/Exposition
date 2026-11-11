import { describe, expect, it } from "vitest";

import { editorDefaults, MONACO_THEMES, monacoThemeData } from "./monaco-theme";
import { fonts, palettes } from "./tokens";

describe("Monaco themes", () => {
  it.each(["light", "dark"] as const)("derives %s colors from tokens in Monaco's hex format", (theme) => {
    const data = monacoThemeData(theme);
    expect(data.base).toBe(theme === "dark" ? "vs-dark" : "vs");
    for (const [key, value] of Object.entries(data.colors)) {
      expect(value, key).toMatch(/^#[0-9a-f]{6}([0-9a-f]{2})?$/);
    }
    expect(data.colors["editor.background"]).toBe(palettes[theme].surface);
    expect(data.colors["focusBorder"]).toBe(palettes[theme]["focus-ring"]);
    for (const rule of data.rules) if (rule.foreground) expect(rule.foreground).toMatch(/^[0-9a-f]{6}$/);
  });

  it("uses the shared code font and metrics", () => {
    expect(editorDefaults.fontFamily).toBe(fonts.code);
    expect(editorDefaults.fontSize).toBe(13);
    expect(editorDefaults.lineHeight).toBe(20);
    expect(MONACO_THEMES).toEqual({ light: "exposition-light", dark: "exposition-dark" });
  });
});
