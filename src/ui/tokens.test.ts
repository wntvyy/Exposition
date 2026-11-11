import { describe, expect, it } from "vitest";

import { contrastRatio, parseColor, readCustomProperties, resolveVar } from "./color";
import { colorRoles, palettes, type ColorRole, type ThemeName } from "./tokens";

// Vitest stubs CSS imports (even `?raw`), so read the stylesheet from disk.
const nodeFs = "node:fs";
const { readFileSync } = (await import(/* @vite-ignore */ nodeFs)) as {
  readFileSync: (path: string, encoding: "utf8") => string;
};
// Vitest runs from the repository root (`npm test`).
const tokensCss = readFileSync("src/ui/tokens.css", "utf8");

function themeProperties(theme: ThemeName): Record<string, string> {
  const base = readCustomProperties(tokensCss, ":root");
  return theme === "light" ? base : { ...base, ...readCustomProperties(tokensCss, ':root[data-theme="dark"]') };
}

function resolvedColor(theme: ThemeName, role: ColorRole): string {
  const props = themeProperties(theme);
  const raw = props[`--color-${role}`];
  if (raw === undefined) throw new Error(`--color-${role} missing from tokens.css`);
  return resolveVar(raw, props);
}

// [foreground, background] pairs. Body text pairs need 4.5:1 (WCAG 1.4.3);
// UI boundaries, focus indicators, and meaningful icons need 3:1 (WCAG 1.4.11).
const surfaces: ColorRole[] = ["surface-base", "surface-sunken", "surface", "surface-raised", "field"];
const interactive: ColorRole[] = ["surface-hover", "surface-selected", "accent-subtle"];

const textPairs: [ColorRole, ColorRole][] = [
  ...[...surfaces, ...interactive, "surface-active" as const].map((bg): [ColorRole, ColorRole] => ["text", bg]),
  ...[...surfaces, ...interactive, "surface-active" as const].map((bg): [ColorRole, ColorRole] => ["text-muted", bg]),
  ...[...surfaces, ...interactive].map((bg): [ColorRole, ColorRole] => ["text-subtle", bg]),
  ...surfaces.map((bg): [ColorRole, ColorRole] => ["accent-text", bg]),
  ["accent-text", "accent-subtle"],
  ["text-on-accent", "accent"],
  ["text-on-accent", "accent-hover"],
  ["text-on-accent", "accent-active"],
  ["text-on-accent", "danger-solid"],
  ["text-on-accent", "danger-solid-hover"],
  ["text", "selection"],
  ["text", "selection-inactive"],
  ...(["danger", "warning", "success", "info"] as const).flatMap((tone): [ColorRole, ColorRole][] => [
    [`${tone}-text`, `${tone}-bg`],
    [`${tone}-text`, "surface"],
    [`${tone}-text`, "surface-sunken"],
    [`${tone}-text`, "surface-raised"],
    ["text", `${tone}-bg`],
  ]),
  ...(["running", "stale", "idle", "unavailable", "canceled"] as const).map(
    (s): [ColorRole, ColorRole] => [`status-${s}-text`, `status-${s}-bg`],
  ),
  ["status-stale-text", "surface"],
  ["status-stale-text", "surface-sunken"],
  ["tooltip-text", "tooltip-bg"],
  ["kbd-text", "kbd-bg"],
  ["statusbar-text", "statusbar-bg"],
  ["text", "tab-active-bg"],
  ["text-muted", "surface-sunken"],
];

const boundaryPairs: [ColorRole, ColorRole][] = [
  ...[...surfaces, "surface-hover" as const].map((bg): [ColorRole, ColorRole] => ["border-strong", bg]),
  ...[...surfaces, "surface-hover" as const, "surface-selected" as const, "accent-subtle" as const].map(
    (bg): [ColorRole, ColorRole] => ["focus-ring", bg],
  ),
  ...surfaces.map((bg): [ColorRole, ColorRole] => ["accent", bg]),
  ["tab-indicator", "tab-active-bg"],
];

describe("design tokens", () => {
  it.each(["light", "dark"] as const)("tokens.ts mirrors tokens.css (%s)", (theme) => {
    for (const role of colorRoles) {
      const css = parseColor(resolvedColor(theme, role));
      const ts = parseColor(palettes[theme][role]);
      expect({ role, ...ts }).toEqual({ role, ...css });
    }
  });

  it("defines no color custom property that tokens.ts lacks", () => {
    const declared = Object.keys(themeProperties("light"))
      .filter((p) => p.startsWith("--color-"))
      .map((p) => p.slice("--color-".length));
    expect(declared.filter((role) => !(colorRoles as readonly string[]).includes(role))).toEqual([]);
  });

  it.each(["light", "dark"] as const)("text pairs meet 4.5:1 (%s)", (theme) => {
    const failures = textPairs
      .map(([fg, bg]) => ({ fg, bg, ratio: contrastRatio(resolvedColor(theme, fg), resolvedColor(theme, bg)) }))
      .filter((r) => r.ratio < 4.5)
      .map((r) => `${r.fg} on ${r.bg}: ${r.ratio.toFixed(2)}`);
    expect(failures).toEqual([]);
  });

  it.each(["light", "dark"] as const)("boundary, focus, and icon pairs meet 3:1 (%s)", (theme) => {
    const failures = boundaryPairs
      .map(([fg, bg]) => ({ fg, bg, ratio: contrastRatio(resolvedColor(theme, fg), resolvedColor(theme, bg)) }))
      .filter((r) => r.ratio < 3)
      .map((r) => `${r.fg} on ${r.bg}: ${r.ratio.toFixed(2)}`);
    expect(failures).toEqual([]);
  });
});

describe("contrast math", () => {
  it("matches WCAG reference values", () => {
    expect(contrastRatio("#000000", "#ffffff")).toBeCloseTo(21, 5);
    expect(contrastRatio("#ffffff", "#ffffff")).toBeCloseTo(1, 5);
    expect(contrastRatio("#767676", "#ffffff")).toBeCloseTo(4.54, 2);
  });

  it("composites translucent foregrounds", () => {
    expect(contrastRatio("rgba(0, 0, 0, 0)", "#ffffff")).toBeCloseTo(1, 5);
    expect(contrastRatio("rgba(0, 0, 0, 1)", "#ffffff")).toBeCloseTo(21, 5);
  });
});
