import { describe, expect, it } from "vitest";

import { contrastRatio, parseColor, readCustomProperties, resolveVar } from "./color";
import { colorRoles, densityPx, graphCategories, palettes, themeKinds, themeNames, type ColorRole, type ThemeName } from "./tokens";

// Vitest stubs CSS imports (even `?raw`), so read the stylesheet from disk.
const nodeFs = "node:fs";
const { readFileSync } = (await import(/* @vite-ignore */ nodeFs)) as {
  readFileSync: (path: string, encoding: "utf8") => string;
};
// Vitest runs from the repository root (`npm test`).
const tokensCss = readFileSync("src/ui/tokens.css", "utf8");

function themeProperties(theme: ThemeName): Record<string, string> {
  const base = readCustomProperties(tokensCss, ":root");
  return theme === "light" ? base : { ...base, ...readCustomProperties(tokensCss, `:root[data-theme="${theme}"]`) };
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
  // Quiet inline status (StatusText): colored icon + text on rows, including
  // hovered and selected rows.
  ...(["danger", "warning", "success", "info"] as const).flatMap((tone): [ColorRole, ColorRole][] =>
    (["surface-hover", "surface-selected", "accent-subtle"] as const).map((bg): [ColorRole, ColorRole] => [`${tone}-text`, bg]),
  ),
  ...(["surface-raised", "surface-hover", "surface-selected", "accent-subtle"] as const).map(
    (bg): [ColorRole, ColorRole] => ["status-stale-text", bg],
  ),
  ["tooltip-text", "tooltip-bg"],
  ["kbd-text", "kbd-bg"],
  ["statusbar-text", "statusbar-bg"],
  ["text", "tab-active-bg"],
  ["text-muted", "surface-sunken"],
  // Graph nodes: the title (text), secondary text, and the type label on each category fill.
  ...graphCategories.flatMap((c): [ColorRole, ColorRole][] => [
    ["text", `graph-${c}-bg`],
    ["text-muted", `graph-${c}-bg`],
    [`graph-${c}-text`, `graph-${c}-bg`],
  ]),
];

const boundaryPairs: [ColorRole, ColorRole][] = [
  ...[...surfaces, "surface-hover" as const].map((bg): [ColorRole, ColorRole] => ["border-strong", bg]),
  ...[...surfaces, "surface-hover" as const, "surface-selected" as const, "accent-subtle" as const].map(
    (bg): [ColorRole, ColorRole] => ["focus-ring", bg],
  ),
  ...surfaces.map((bg): [ColorRole, ColorRole] => ["accent", bg]),
  ["tab-indicator", "tab-active-bg"],
  // Graph: node outlines and edges are meaningful boundaries on the canvas; the
  // selection/focus ring must show on every node fill.
  ...graphCategories.flatMap((c): [ColorRole, ColorRole][] => [
    [`graph-${c}-border`, "graph-canvas"],
    [`graph-${c}-border`, `graph-${c}-bg`],
    ["focus-ring", `graph-${c}-bg`],
  ]),
  ...(["graph-edge", "graph-edge-math", "graph-edge-reference", "graph-edge-evidence", "graph-edge-formal"] as const).map(
    (edge): [ColorRole, ColorRole] => [edge, "graph-canvas"],
  ),
  ["focus-ring", "graph-canvas"],
];

describe("design tokens", () => {
  it.each(themeNames)("tokens.ts mirrors tokens.css (%s)", (theme) => {
    for (const role of colorRoles) {
      const css = parseColor(resolvedColor(theme, role));
      const ts = parseColor(palettes[theme][role]);
      expect({ role, ...ts }).toEqual({ role, ...css });
    }
  });

  it.each(themeNames.filter((t) => t !== "light"))("%s overrides every base color role and sets its color scheme", (theme) => {
    const block = readCustomProperties(tokensCss, `:root[data-theme="${theme}"]`);
    const rootProps = readCustomProperties(tokensCss, ":root");
    const baseRoles = colorRoles.filter((role) => !rootProps[`--color-${role}`]?.startsWith("var("));
    expect(baseRoles.filter((role) => block[`--color-${role}`] === undefined)).toEqual([]);
    expect(tokensCss).toContain(`:root[data-theme="${theme}"] {\n  color-scheme: ${themeKinds[theme]};`);
  });

  it("keeps the semantic roles apart from the accent in every theme", () => {
    for (const theme of themeNames) {
      const c = palettes[theme];
      for (const role of ["info-text", "success-text", "warning-text", "danger-text", "focus-ring"] as const) {
        expect([theme, role, c[role]]).not.toEqual([theme, role, c["accent-text"]]);
      }
      // Stale follows the warning colors (one hue fewer).
      expect(c["status-stale-text"]).toBe(c["warning-text"]);
    }
  });

  it.each(themeNames)("keeps every pair of graph categories distinguishable (%s)", (theme) => {
    // CIE76 ΔE between node outlines/edge colors; 15 is clearly visible side by side.
    const lab = (value: string) => {
      const { r, g, b } = parseColor(value);
      const lin = [r, g, b].map((x) => {
        const v = x / 255;
        return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
      }) as [number, number, number];
      const X = (lin[0] * 0.4124 + lin[1] * 0.3576 + lin[2] * 0.1805) / 0.95047;
      const Y = lin[0] * 0.2126 + lin[1] * 0.7152 + lin[2] * 0.0722;
      const Z = (lin[0] * 0.0193 + lin[1] * 0.1192 + lin[2] * 0.9505) / 1.08883;
      const f = (t: number) => (t > 0.008856 ? Math.cbrt(t) : 7.787 * t + 16 / 116);
      return [116 * f(Y) - 16, 500 * (f(X) - f(Y)), 200 * (f(Y) - f(Z))];
    };
    const close: string[] = [];
    for (const [i, a] of graphCategories.entries()) {
      for (const b of graphCategories.slice(i + 1)) {
        const [la, lb] = [lab(palettes[theme][`graph-${a}-border`]), lab(palettes[theme][`graph-${b}-border`])];
        const dE = Math.hypot(la[0]! - lb[0]!, la[1]! - lb[1]!, la[2]! - lb[2]!);
        if (dE < 15) close.push(`${a}/${b}: ${dE.toFixed(1)}`);
      }
    }
    expect(close).toEqual([]);
  });

  it("defines no color custom property that tokens.ts lacks", () => {
    const declared = Object.keys(themeProperties("light"))
      .filter((p) => p.startsWith("--color-"))
      .map((p) => p.slice("--color-".length));
    expect(declared.filter((role) => !(colorRoles as readonly string[]).includes(role))).toEqual([]);
  });

  it.each(themeNames)("text pairs meet 4.5:1 (%s)", (theme) => {
    const failures = textPairs
      .map(([fg, bg]) => ({ fg, bg, ratio: contrastRatio(resolvedColor(theme, fg), resolvedColor(theme, bg)) }))
      .filter((r) => r.ratio < 4.5)
      .map((r) => `${r.fg} on ${r.bg}: ${r.ratio.toFixed(2)}`);
    expect(failures).toEqual([]);
  });

  it.each(themeNames)("boundary, focus, and icon pairs meet 3:1 (%s)", (theme) => {
    const failures = boundaryPairs
      .map(([fg, bg]) => ({ fg, bg, ratio: contrastRatio(resolvedColor(theme, fg), resolvedColor(theme, bg)) }))
      .filter((r) => r.ratio < 3)
      .map((r) => `${r.fg} on ${r.bg}: ${r.ratio.toFixed(2)}`);
    expect(failures).toEqual([]);
  });
});

describe("density tokens", () => {
  it("densityPx mirrors tokens.css", () => {
    const props = themeProperties("light");
    const px = (name: string) => Number.parseFloat(resolveVar(props[name] ?? "", props));
    expect({
      controlSm: px("--control-height-sm"),
      controlMd: px("--control-height-md"),
      controlLg: px("--control-height-lg"),
      rowTree: px("--row-height-tree"),
      rowTable: px("--row-height-table"),
      rowMenu: px("--row-height-menu"),
      tab: px("--tab-height"),
      paneHeader: px("--pane-header-height"),
      pageBar: px("--page-bar-height"),
      statusbar: px("--statusbar-height"),
    }).toEqual(densityPx);
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

describe("shared component insets", () => {
  // jsdom does not resolve custom properties, so guard the rule shape instead:
  // an element that pads itself with var(--section-inset) must not reset that
  // variable on itself (content sections lost their inset once).
  const componentsCss = readFileSync("src/ui/components/components.css", "utf8");
  it("resets --section-inset only below a content section body, never on the body itself", () => {
    const rules = [...componentsCss.matchAll(/([^{}]+)\{([^{}]*)\}/g)];
    const resetting = rules.filter(([, , body]) => /--section-inset:\s*0/.test(body ?? ""));
    expect(resetting.length).toBeGreaterThan(0);
    for (const [, selectors] of resetting) {
      for (const selector of (selectors ?? "").split(",").map((s) => s.replace(/\/\*[\s\S]*?\*\//g, "").trim())) {
        expect(selector).not.toMatch(/\.ui-section__body$/);
      }
    }
  });
});
