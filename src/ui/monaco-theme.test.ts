import { describe, expect, it } from "vitest";

import { editorDefaults, MONACO_THEMES, monacoThemeData } from "./monaco-theme";
import { fonts, palettes, themeKinds, themeNames } from "./tokens";

describe("Monaco themes", () => {
  it.each(themeNames)("derives %s colors from tokens in Monaco's hex format", (theme) => {
    const data = monacoThemeData(theme);
    expect(data.base).toBe(themeKinds[theme] === "dark" ? "vs-dark" : "vs");
    for (const [key, value] of Object.entries(data.colors)) {
      expect(value, key).toMatch(/^#[0-9a-f]{6}([0-9a-f]{2})?$/);
    }
    expect(data.colors["editor.background"]).toBe(palettes[theme].surface);
    expect(data.colors["focusBorder"]).toBe(palettes[theme]["focus-ring"]);
    for (const rule of data.rules) if (rule.foreground) expect(rule.foreground).toMatch(/^[0-9a-f]{6}$/);
    // Keywords use the info role, not the accent (approved 2026-11-19).
    expect(data.rules.find((r) => r.token === "keyword")?.foreground).toBe(palettes[theme]["info-text"].slice(1));
  });

  // UI-14: a fenced code block in rendered Markdown colors the same roles as the editor.
  it("Markdown code tokens use the same color roles as the Monaco rules", async () => {
    const nodeFs = "node:fs";
    const { readFileSync } = (await import(/* @vite-ignore */ nodeFs)) as { readFileSync: (path: string, encoding: "utf8") => string };
    const css = readFileSync("src/ui/components/Markdown.css", "utf8");
    const roleOf = (token: string): string | undefined => {
      for (const block of css.matchAll(/((?:\.ui-md-tok-[a-z]+,?\s*)+)\{([^}]*)\}/g)) {
        if (block[1]!.split(",").some((s) => s.trim() === `.ui-md-tok-${token}`)) return /color:\s*var\(--color-([a-z-]+)\)/.exec(block[2]!)?.[1];
      }
      return undefined;
    };
    const monacoRole = (token: string): string | undefined => {
      const foreground = monacoThemeData("light").rules.find((r) => r.token === token)?.foreground;
      return (Object.keys(palettes.light) as (keyof typeof palettes.light)[]).filter((role) => role.endsWith("-text") || role.startsWith("text")).find((role) => palettes.light[role].slice(1, 7) === foreground);
    };
    for (const [md, monaco] of [
      ["comment", "comment"],
      ["keyword", "keyword"],
      ["string", "string"],
      ["number", "number"],
      ["type", "type"],
      ["tag", "tag"],
      ["attribute", "attribute.name"],
      ["delimiter", "delimiter"],
      ["operator", "operator"],
      ["invalid", "invalid"],
    ] as const) {
      expect([md, roleOf(md)]).toEqual([md, monacoRole(monaco)]);
    }
    expect(roleOf("keyword")).toBe("info-text");
    expect(roleOf("type")).toBe("accent-text");
    expect(css).not.toMatch(/ui-md-tok[^}]*\{[^}]*stale/);
  });

  it("uses the shared code font and metrics", () => {
    expect(editorDefaults.fontFamily).toBe(fonts.code);
    expect(editorDefaults.fontSize).toBe(13);
    expect(editorDefaults.lineHeight).toBe(20);
    expect(MONACO_THEMES).toEqual({ light: "exposition-light", dark: "exposition-dark", ocean: "exposition-ocean", sage: "exposition-sage" });
  });
});
