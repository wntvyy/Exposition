import { describe, expect, it } from "vitest";

import { contrastRatio, parseColor } from "../../color";
import { colorRoles } from "../../tokens";
import { CONTRAST_PAIRS, evaluateContrast, PALETTE_IDS, paletteStyle, V2_PALETTES } from "./palettes";

describe("V2 sample palettes", () => {
  it("defines exactly the four themes", () => {
    expect(PALETTE_IDS).toEqual(["dark", "light", "ocean", "sage"]);
    expect(PALETTE_IDS.map((id) => V2_PALETTES[id].kind)).toEqual(["dark", "light", "dark", "light"]);
  });

  it.each(PALETTE_IDS)("%s uses exactly the production color roles, with parseable values", (id) => {
    const colors = V2_PALETTES[id].colors;
    expect(Object.keys(colors).sort()).toEqual([...colorRoles].sort());
    for (const role of colorRoles) expect(() => parseColor(colors[role])).not.toThrow();
  });

  it.each(PALETTE_IDS)("%s meets 4.5:1 for every text pair and 3:1 for every boundary and focus pair", (id) => {
    const failures = evaluateContrast(V2_PALETTES[id])
      .filter((r) => !r.pass)
      .map((r) => `${r.fg} on ${r.bg}: ${r.ratio.toFixed(2)} < ${r.min}`);
    expect(failures).toEqual([]);
  });

  it.each(PALETTE_IDS)("%s keeps semantic roles distinct from the accent", (id) => {
    const c = V2_PALETTES[id].colors;
    for (const role of ["focus-ring", "info-text", "success-text", "warning-text", "danger-text", "selection"] as const) {
      expect(c[role].toLowerCase()).not.toBe(c.accent.toLowerCase());
      expect(c[role].toLowerCase()).not.toBe(c["accent-text"].toLowerCase());
    }
    // Focus and info stay visibly apart from the accent text (not just a hex step).
    expect(hueDistance(c["focus-ring"], c["accent-text"]) + lightnessDistance(c["focus-ring"], c["accent-text"])).toBeGreaterThan(0.04);
    expect(hueDistance(c["info-text"], c["accent-text"]) + lightnessDistance(c["info-text"], c["accent-text"])).toBeGreaterThan(0.04);
  });

  it("keeps Sage's success green apart from its gray-green accent", () => {
    const c = V2_PALETTES.sage.colors;
    expect(hueDistance(c["success-text"], c["accent-text"])).toBeGreaterThan(0.06);
    // The colour distance itself is guarded for every theme in tokens.test.ts.
  });

  it.each(PALETTE_IDS)("%s stays restrained: muted accent, near-neutral tints, stale in the warning family", (id) => {
    const c = V2_PALETTES[id].colors;
    for (const role of ["accent", "accent-text"] as const) expect(chroma(c[role])).toBeLessThan(0.36);
    for (const tone of ["danger", "warning", "success", "info"] as const) expect(chroma(c[`${tone}-bg`])).toBeLessThan(0.07);
    expect(c["status-stale-text"]).toBe(c["warning-text"]);
    expect(c["status-stale-bg"]).toBe(c["warning-bg"]);
  });

  it("applies a palette as local custom properties only", () => {
    const style = paletteStyle(V2_PALETTES.ocean) as Record<string, string>;
    expect(style["--color-accent"]).toBe(V2_PALETTES.ocean.colors.accent);
    expect(Object.keys(style).filter((k) => k.startsWith("--color-"))).toHaveLength(colorRoles.length);
  });

  it("documents both kinds of pairs", () => {
    expect(CONTRAST_PAIRS.some((p) => p.kind === "text")).toBe(true);
    expect(CONTRAST_PAIRS.some((p) => p.kind === "boundary")).toBe(true);
    expect(contrastRatio("#000000", "#ffffff")).toBeCloseTo(21, 0);
  });
});

function hsl(hex: string): [number, number] {
  const { r, g, b } = parseColor(hex);
  const [R, G, B] = [r / 255, g / 255, b / 255];
  const max = Math.max(R, G, B);
  const min = Math.min(R, G, B);
  const l = (max + min) / 2;
  if (max === min) return [0, l];
  const d = max - min;
  let h = max === R ? (G - B) / d + (G < B ? 6 : 0) : max === G ? (B - R) / d + 2 : (R - G) / d + 4;
  h /= 6;
  return [h, l];
}
function hueDistance(a: string, b: string): number {
  const d = Math.abs(hsl(a)[0] - hsl(b)[0]);
  return Math.min(d, 1 - d);
}
/** RGB chroma (max - min channel), 0..1. */
function chroma(hex: string): number {
  const { r, g, b } = parseColor(hex);
  return (Math.max(r, g, b) - Math.min(r, g, b)) / 255;
}
function lightnessDistance(a: string, b: string): number {
  return Math.abs(hsl(a)[1] - hsl(b)[1]);
}
