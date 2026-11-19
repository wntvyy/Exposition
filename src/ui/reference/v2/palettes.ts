// V2 review samples: the four palettes (approved 2026-11-19).
//
// Since the selection (2026-11-19) these are the production palettes: the
// colors come from `src/ui/tokens.ts` (mirrored by `tokens.css`); this module
// only adds the sheet's names, summaries, and the documented contrast pairs.
//
// Revised 2026-11-19 at the user's request: less vibrant, more neutral, fewer
// colors. Each theme uses one accent and four muted semantic hues (info,
// success, warning, danger); stale joins the warning family; graph categories
// reuse those hues, plus one muted violet for math nodes only. Values are
// solved against the contrast pairs below rather than hand-tuned.
//
// Contrast is computed with `src/ui/color.ts` for the documented pairs below
// (text 4.5:1, boundaries and focus 3:1); `palettes.test.ts` fails on any miss.
import type { CSSProperties } from "react";

import { contrastRatio } from "../../color";
import { graphCategories, palettes, type ColorRole } from "../../tokens";

export type PaletteId = "dark" | "light" | "ocean" | "sage";
export type PaletteKind = "light" | "dark";

export interface V2Palette {
  id: PaletteId;
  name: string;
  kind: PaletteKind;
  /** One line for the sheet header. */
  summary: string;
  colors: Record<ColorRole, string>;
}

export const PALETTE_IDS: PaletteId[] = ["dark", "light", "ocean", "sage"];

export const V2_PALETTES: Record<PaletteId, V2Palette> = {
  dark: {
    id: "dark",
    name: "Dark",
    kind: "dark",
    summary: "Neutral dark surfaces with a muted slate-blue accent.",
    colors: palettes.dark,
  },
  light: {
    id: "light",
    name: "Light",
    kind: "light",
    summary: "Neutral light surfaces, white documents, a muted slate-blue accent.",
    colors: palettes.light,
  },
  ocean: {
    id: "ocean",
    name: "Ocean",
    kind: "dark",
    summary: "Gray-navy surfaces with a muted teal accent; focus and info stay blue.",
    colors: palettes.ocean,
  },
  sage: {
    id: "sage",
    name: "Sage",
    kind: "light",
    summary: "Warm neutral light surfaces with a gray-green accent; focus and info stay blue.",
    colors: palettes.sage,
  },
};

/** Custom-property overrides that apply a palette to a sample root only. */
export function paletteStyle(palette: V2Palette): CSSProperties {
  const style: Record<string, string> = { colorScheme: palette.kind };
  for (const [role, value] of Object.entries(palette.colors)) style[`--color-${role}`] = value;
  return style as CSSProperties;
}

// ---- Contrast pairs ([foreground, background]); the same documented set as
// `tokens.test.ts`, plus the V2 additions (selected rows, diagnostics on
// selected rows, accent text on hover, syntax roles on the editor surface).

export type PairKind = "text" | "boundary";
export interface ContrastPair {
  fg: ColorRole;
  bg: ColorRole;
  kind: PairKind;
  /** What the pair is used for (shown in the sheet). */
  use: string;
}

const surfaces: ColorRole[] = ["surface-base", "surface-sunken", "surface", "surface-raised", "field"];
const interactive: ColorRole[] = ["surface-hover", "surface-selected", "accent-subtle"];
const tones = ["danger", "warning", "success", "info"] as const;

function text(fg: ColorRole, bg: ColorRole, use: string): ContrastPair {
  return { fg, bg, kind: "text", use };
}
function boundary(fg: ColorRole, bg: ColorRole, use: string): ContrastPair {
  return { fg, bg, kind: "boundary", use };
}

export const CONTRAST_PAIRS: ContrastPair[] = [
  ...[...surfaces, ...interactive, "surface-active" as const].map((bg) => text("text", bg, "Body text")),
  ...[...surfaces, ...interactive, "surface-active" as const].map((bg) => text("text-muted", bg, "Secondary text")),
  ...[...surfaces, ...interactive].map((bg) => text("text-subtle", bg, "Meta, line numbers, comments")),
  ...surfaces.map((bg) => text("accent-text", bg, "Links, accent text")),
  text("accent-text", "accent-subtle", "Accent text on focused selection"),
  text("accent-text", "surface-hover", "Link in a hovered row"),
  text("text-on-accent", "accent", "Primary button"),
  text("text-on-accent", "accent-hover", "Primary button, hover"),
  text("text-on-accent", "accent-active", "Primary button, pressed"),
  text("text-on-accent", "danger-solid", "Destructive button"),
  text("text-on-accent", "danger-solid-hover", "Destructive button, hover"),
  text("text", "selection", "Selected text"),
  text("text", "selection-inactive", "Selected text, unfocused"),
  ...tones.flatMap((tone): ContrastPair[] => [
    text(`${tone}-text`, `${tone}-bg`, `${tone} in a tinted banner`),
    text(`${tone}-text`, "surface", `${tone} diagnostic on the editor`),
    text(`${tone}-text`, "surface-sunken", `${tone} in a panel`),
    text(`${tone}-text`, "surface-raised", `${tone} in a menu or dialog`),
    text("text", `${tone}-bg`, `Body text in a ${tone} banner`),
    text(`${tone}-text`, "surface-hover", `${tone} status in a hovered row`),
    text(`${tone}-text`, "surface-selected", `${tone} status in a selected row`),
    text(`${tone}-text`, "accent-subtle", `${tone} status in a focused selected row`),
  ]),
  ...(["running", "stale", "idle", "unavailable", "canceled"] as const).map((s) =>
    text(`status-${s}-text`, `status-${s}-bg`, `Status chip: ${s}`),
  ),
  ...(["surface", "surface-sunken", "surface-raised", "surface-hover", "surface-selected", "accent-subtle"] as const).map((bg) =>
    text("status-stale-text", bg, "Stale status, types in code"),
  ),
  text("tooltip-text", "tooltip-bg", "Tooltip"),
  text("kbd-text", "kbd-bg", "Key cap"),
  text("statusbar-text", "statusbar-bg", "Status bar"),
  text("text", "tab-active-bg", "Active tab"),
  ...graphCategories.flatMap((c): ContrastPair[] => [
    text("text", `graph-${c}-bg`, `Node title on ${c}`),
    text("text-muted", `graph-${c}-bg`, `Node meta on ${c}`),
    text(`graph-${c}-text`, `graph-${c}-bg`, `Type label on ${c}`),
  ]),
  ...[...surfaces, "surface-hover" as const].map((bg) => boundary("border-strong", bg, "Control boundary")),
  ...[...surfaces, "surface-hover" as const, "surface-selected" as const, "accent-subtle" as const].map((bg) =>
    boundary("focus-ring", bg, "Focus ring"),
  ),
  ...surfaces.map((bg) => boundary("accent", bg, "Accent fill, toggles")),
  boundary("tab-indicator", "tab-active-bg", "Active tab indicator"),
  boundary("danger-text", "field", "Field in error (border)"),
  ...graphCategories.flatMap((c): ContrastPair[] => [
    boundary(`graph-${c}-border`, "graph-canvas", `${c} outline and edges`),
    boundary(`graph-${c}-border`, `graph-${c}-bg`, `${c} band edge`),
    boundary("focus-ring", `graph-${c}-bg`, `Focus on ${c}`),
  ]),
  ...(["graph-edge", "graph-edge-math", "graph-edge-reference", "graph-edge-evidence", "graph-edge-formal"] as const).map((edge) =>
    boundary(edge, "graph-canvas", "Edge on the canvas"),
  ),
  boundary("focus-ring", "graph-canvas", "Focus on the canvas"),
];

export interface ContrastResult extends ContrastPair {
  ratio: number;
  min: number;
  pass: boolean;
}

export function evaluateContrast(palette: V2Palette): ContrastResult[] {
  return CONTRAST_PAIRS.map((pair) => {
    const ratio = contrastRatio(palette.colors[pair.fg], palette.colors[pair.bg]);
    const min = pair.kind === "text" ? 4.5 : 3;
    return { ...pair, ratio, min, pass: ratio >= min };
  });
}
