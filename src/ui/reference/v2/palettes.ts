// V2 review samples (phase 1): the four proposed palettes.
//
// Sample-only. Nothing here is applied to the application: production colors
// stay in `src/ui/tokens.css`. Each palette uses exactly the production color
// role names (`ColorRole`, `--color-<role>`), so a chosen palette maps 1:1 onto
// `tokens.css` later. Aliases follow the same derivations as `tokens.css`
// (status-running = info, idle/unavailable/canceled = neutral, status bar =
// surface-base + text-muted, graph edges = category borders).
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
import { graphCategories, type ColorRole } from "../../tokens";

export type PaletteId = "dark" | "light" | "ocean" | "sage";
export type PaletteKind = "light" | "dark";

type AliasRole =
  | "status-running-text"
  | "status-running-bg"
  | "status-running-border"
  | "status-idle-text"
  | "status-idle-bg"
  | "status-unavailable-text"
  | "status-unavailable-bg"
  | "status-canceled-text"
  | "status-canceled-bg"
  | "statusbar-bg"
  | "statusbar-text"
  | "tab-active-bg"
  | "tab-indicator"
  | "graph-canvas"
  | "graph-edge-math"
  | "graph-edge-reference"
  | "graph-edge-evidence"
  | "graph-edge-formal";

type BaseColors = Record<Exclude<ColorRole, AliasRole>, string>;

function withAliases(base: BaseColors): Record<ColorRole, string> {
  return {
    ...base,
    "status-running-text": base["info-text"],
    "status-running-bg": base["info-bg"],
    "status-running-border": base["info-border"],
    "status-idle-text": base["status-neutral-text"],
    "status-idle-bg": base["status-neutral-bg"],
    "status-unavailable-text": base["status-neutral-text"],
    "status-unavailable-bg": base["status-neutral-bg"],
    "status-canceled-text": base["status-neutral-text"],
    "status-canceled-bg": base["status-neutral-bg"],
    "statusbar-bg": base["surface-base"],
    "statusbar-text": base["text-muted"],
    "tab-active-bg": base.surface,
    "tab-indicator": base.accent,
    "graph-canvas": base["surface-sunken"],
    "graph-edge-math": base["graph-math-border"],
    "graph-edge-reference": base["graph-reference-border"],
    "graph-edge-evidence": base["graph-data-border"],
    "graph-edge-formal": base["graph-formal-border"],
  };
}

export interface V2Palette {
  id: PaletteId;
  name: string;
  kind: PaletteKind;
  /** One line for the sheet header. */
  summary: string;
  colors: Record<ColorRole, string>;
}

const light: BaseColors = {
  "surface-base": "#edeeef",
  "surface-sunken": "#f6f7f7",
  surface: "#ffffff",
  "surface-raised": "#ffffff",
  "surface-hover": "#ebebed",
  "surface-active": "#e1e2e5",
  "surface-selected": "#e5e7eb",
  field: "#ffffff",
  overlay: "rgba(15, 18, 24, 0.28)",
  "accent-subtle": "#e8ecf3",
  text: "#1e2024",
  "text-muted": "#4b4f58",
  "text-subtle": "#5e636e",
  "text-disabled": "#a0a4ab",
  "text-on-accent": "#ffffff",
  border: "#e0e1e3",
  "border-subtle": "#ecedee",
  "border-strong": "#7f848e",
  accent: "#476490",
  "accent-hover": "#415b84",
  "accent-active": "#3b5378",
  "accent-text": "#3c5a86",
  "focus-ring": "#3671c9",
  selection: "#d0dbec",
  "selection-inactive": "#e5e6e8",
  "danger-bg": "#f8f2f2",
  "danger-border": "#e2cccb",
  "danger-text": "#84302a",
  "warning-bg": "#f8f5f2",
  "warning-border": "#e2d7cb",
  "warning-text": "#8d5817",
  "success-bg": "#f2f8f4",
  "success-border": "#cbe2d4",
  "success-text": "#33714c",
  "info-bg": "#f2f7f8",
  "info-border": "#cbdee2",
  "info-text": "#276e7c",
  "danger-solid": "#b03930",
  "danger-solid-hover": "#9c322b",
  "status-stale-text": "#8d5817",
  "status-stale-bg": "#f8f5f2",
  "status-stale-border": "#e2d7cb",
  "status-neutral-text": "#4b4f58",
  "status-neutral-bg": "#f0f1f2",
  "status-neutral-border": "#d9dbdd",
  "tooltip-bg": "#1e2024",
  "tooltip-text": "#f4f5f5",
  "kbd-bg": "#f0f1f2",
  "kbd-border": "#ced0d4",
  "kbd-text": "#4b4f58",
  scrollbar: "rgba(94, 99, 110, 0.32)",
  "scrollbar-hover": "rgba(94, 99, 110, 0.52)",
  shadow: "rgba(16, 20, 28, 0.12)",
  "shadow-strong": "rgba(16, 20, 28, 0.22)",
  "graph-grid": "#dcdde0",
  "graph-structure-bg": "#f1f2f3",
  "graph-structure-border": "#6d737e",
  "graph-structure-text": "#4c5057",
  "graph-document-bg": "#eef1f7",
  "graph-document-border": "#4d6d9d",
  "graph-document-text": "#364c6d",
  "graph-data-bg": "#eef6f2",
  "graph-data-border": "#51976e",
  "graph-data-text": "#396a4e",
  "graph-math-bg": "#f1eff5",
  "graph-math-border": "#6e5991",
  "graph-math-text": "#4c3e65",
  "graph-formal-bg": "#f7f3ee",
  "graph-formal-border": "#ba7b2e",
  "graph-formal-text": "#835621",
  "graph-reference-bg": "#f6eef0",
  "graph-reference-border": "#985264",
  "graph-reference-text": "#6a3945",
  "graph-workflow-bg": "#eef5f7",
  "graph-workflow-border": "#4694a3",
  "graph-workflow-text": "#316772",
  "graph-edge": "#787d87",
};

const dark: BaseColors = {
  "surface-base": "#151517",
  "surface-sunken": "#18191b",
  surface: "#1d1e20",
  "surface-raised": "#26272a",
  "surface-hover": "#28292c",
  "surface-active": "#2f3134",
  "surface-selected": "#2a2e35",
  field: "#212224",
  overlay: "rgba(0, 0, 0, 0.5)",
  "accent-subtle": "#27313f",
  text: "#e0e2e5",
  "text-muted": "#adb1b8",
  "text-subtle": "#979ca5",
  "text-disabled": "#666970",
  "text-on-accent": "#ffffff",
  border: "#2d2e32",
  "border-subtle": "#232427",
  "border-strong": "#72767e",
  accent: "#5172a4",
  "accent-hover": "#4b6a98",
  "accent-active": "#45618c",
  "accent-text": "#8babda",
  "focus-ring": "#5c95eb",
  selection: "#344865",
  "selection-inactive": "#34363a",
  "danger-bg": "#2c1d1c",
  "danger-border": "#553734",
  "danger-text": "#e4918b",
  "warning-bg": "#2c251c",
  "warning-border": "#554834",
  "warning-text": "#e1c08e",
  "success-bg": "#1c2c21",
  "success-border": "#34553f",
  "success-text": "#9fd0b0",
  "info-bg": "#1c282c",
  "info-border": "#344f55",
  "info-text": "#97cbd8",
  "danger-solid": "#b4362d",
  "danger-solid-hover": "#9f3028",
  "status-stale-text": "#e1c08e",
  "status-stale-bg": "#2c251c",
  "status-stale-border": "#554834",
  "status-neutral-text": "#adb1b8",
  "status-neutral-bg": "#28292c",
  "status-neutral-border": "#3e4044",
  "tooltip-bg": "#45474c",
  "tooltip-text": "#f4f5f5",
  "kbd-bg": "#2f3134",
  "kbd-border": "#474a4f",
  "kbd-text": "#c9cbcf",
  scrollbar: "rgba(151, 156, 165, 0.3)",
  "scrollbar-hover": "rgba(151, 156, 165, 0.5)",
  shadow: "rgba(0, 0, 0, 0.35)",
  "shadow-strong": "rgba(0, 0, 0, 0.55)",
  "graph-grid": "#34363a",
  "graph-structure-bg": "#26282c",
  "graph-structure-border": "#8c919b",
  "graph-structure-text": "#bec0c6",
  "graph-document-bg": "#1e2733",
  "graph-document-border": "#638ac5",
  "graph-document-text": "#a6bcde",
  "graph-data-bg": "#1e3329",
  "graph-data-border": "#74b494",
  "graph-data-text": "#afd4c2",
  "graph-math-bg": "#261e33",
  "graph-math-border": "#8a6fb8",
  "graph-math-text": "#bcadd7",
  "graph-formal-bg": "#332a1e",
  "graph-formal-border": "#c99b5e",
  "graph-formal-text": "#e0c6a3",
  "graph-reference-bg": "#331e25",
  "graph-reference-border": "#bd6b86",
  "graph-reference-text": "#d9abba",
  "graph-workflow-bg": "#1e3033",
  "graph-workflow-border": "#6bafbd",
  "graph-workflow-text": "#abd1d9",
  "graph-edge": "#7e838b",
};

const ocean: BaseColors = {
  "surface-base": "#10151b",
  "surface-sunken": "#141a21",
  surface: "#181f28",
  "surface-raised": "#1f2733",
  "surface-hover": "#212a36",
  "surface-active": "#283341",
  "surface-selected": "#26323e",
  field: "#1b232c",
  overlay: "rgba(2, 6, 12, 0.55)",
  "accent-subtle": "#233636",
  text: "#dfe2e7",
  "text-muted": "#a9b1bc",
  "text-subtle": "#959fac",
  "text-disabled": "#516885",
  "text-on-accent": "#ffffff",
  border: "#26303e",
  "border-subtle": "#1d252f",
  "border-strong": "#5e7799",
  accent: "#417f7c",
  "accent-hover": "#3b7370",
  "accent-active": "#356865",
  "accent-text": "#98cdc8",
  "focus-ring": "#5a9eed",
  selection: "#324b67",
  "selection-inactive": "#2c3847",
  "danger-bg": "#2e1f1e",
  "danger-border": "#593736",
  "danger-text": "#e48e8b",
  "warning-bg": "#2e281e",
  "warning-border": "#594b36",
  "warning-text": "#e1c08e",
  "success-bg": "#1e2e22",
  "success-border": "#36593f",
  "success-text": "#a1ceac",
  "info-bg": "#1e262e",
  "info-border": "#364659",
  "info-text": "#8db5e2",
  "danger-solid": "#b4312d",
  "danger-solid-hover": "#9f2c28",
  "status-stale-text": "#e1c08e",
  "status-stale-bg": "#2e281e",
  "status-stale-border": "#594b36",
  "status-neutral-text": "#a9b1bc",
  "status-neutral-bg": "#212a36",
  "status-neutral-border": "#334154",
  "tooltip-bg": "#39495d",
  "tooltip-text": "#f2f4f7",
  "kbd-bg": "#283341",
  "kbd-border": "#3b4b60",
  "kbd-text": "#c0cad8",
  scrollbar: "rgba(149, 159, 172, 0.3)",
  "scrollbar-hover": "rgba(149, 159, 172, 0.5)",
  shadow: "rgba(0, 0, 0, 0.4)",
  "shadow-strong": "rgba(0, 0, 0, 0.6)",
  "graph-grid": "#2c3847",
  "graph-structure-bg": "#252b31",
  "graph-structure-border": "#8592a3",
  "graph-structure-text": "#b9c1ca",
  "graph-document-bg": "#212b36",
  "graph-document-border": "#5e90c9",
  "graph-document-text": "#a3c0e0",
  "graph-data-bg": "#21362b",
  "graph-data-border": "#74b494",
  "graph-data-text": "#afd4c2",
  "graph-math-bg": "#282136",
  "graph-math-border": "#8a6fb8",
  "graph-math-text": "#bcadd7",
  "graph-formal-bg": "#362d21",
  "graph-formal-border": "#c99b5e",
  "graph-formal-text": "#e0c6a3",
  "graph-reference-bg": "#362128",
  "graph-reference-border": "#bd6b86",
  "graph-reference-text": "#d9abba",
  "graph-workflow-bg": "#213635",
  "graph-workflow-border": "#74b4b2",
  "graph-workflow-text": "#afd4d3",
  "graph-edge": "#6781a2",
};

const sage: BaseColors = {
  "surface-base": "#e8e8e5",
  "surface-sunken": "#f2f2f0",
  surface: "#fbfbfb",
  "surface-raised": "#ffffff",
  "surface-hover": "#e6e6e2",
  "surface-active": "#deded9",
  "surface-selected": "#e1e5df",
  field: "#ffffff",
  overlay: "rgba(28, 30, 24, 0.28)",
  "accent-subtle": "#e0e8e3",
  text: "#22221b",
  "text-muted": "#59594a",
  "text-subtle": "#646454",
  "text-disabled": "#a7a79a",
  "text-on-accent": "#ffffff",
  border: "#ddddd7",
  "border-subtle": "#e8e8e5",
  "border-strong": "#818170",
  accent: "#5d796b",
  "accent-hover": "#556f62",
  "accent-active": "#4e6559",
  "accent-text": "#4c695b",
  "focus-ring": "#3876c7",
  selection: "#d1dceb",
  "selection-inactive": "#e1e1dd",
  "danger-bg": "#f6efef",
  "danger-border": "#ddc7c5",
  "danger-text": "#82332b",
  "warning-bg": "#f6f3ef",
  "warning-border": "#ddd3c5",
  "warning-text": "#895818",
  "success-bg": "#f0f6ef",
  "success-border": "#c8ddc5",
  "success-text": "#346f2b",
  "info-bg": "#eff2f6",
  "info-border": "#c5d0dd",
  "info-text": "#2f547f",
  "danger-solid": "#ae3d32",
  "danger-solid-hover": "#9a362d",
  "status-stale-text": "#895818",
  "status-stale-bg": "#f6f3ef",
  "status-stale-border": "#ddd3c5",
  "status-neutral-text": "#59594a",
  "status-neutral-bg": "#ebebe8",
  "status-neutral-border": "#d7d7d1",
  "tooltip-bg": "#25251e",
  "tooltip-text": "#f6f6f4",
  "kbd-bg": "#ebebe8",
  "kbd-border": "#cdcdc6",
  "kbd-text": "#59594a",
  scrollbar: "rgba(100, 100, 84, 0.32)",
  "scrollbar-hover": "rgba(100, 100, 84, 0.52)",
  shadow: "rgba(30, 32, 25, 0.12)",
  "shadow-strong": "rgba(30, 32, 25, 0.22)",
  "graph-grid": "#d9d9d3",
  "graph-structure-bg": "#f1f1ef",
  "graph-structure-border": "#7c7c6e",
  "graph-structure-text": "#56564d",
  "graph-document-bg": "#ebeff5",
  "graph-document-border": "#4b72a0",
  "graph-document-text": "#34506f",
  "graph-data-bg": "#ecf4eb",
  "graph-data-border": "#5a9651",
  "graph-data-text": "#406a39",
  "graph-math-bg": "#efecf3",
  "graph-math-border": "#6e5b8f",
  "graph-math-text": "#4d4064",
  "graph-formal-bg": "#f5f0eb",
  "graph-formal-border": "#b47c33",
  "graph-formal-text": "#7f5824",
  "graph-reference-bg": "#f4ebee",
  "graph-reference-border": "#965465",
  "graph-reference-text": "#683b46",
  "graph-workflow-bg": "#edf2f0",
  "graph-workflow-border": "#638875",
  "graph-workflow-text": "#455f52",
  "graph-edge": "#888877",
};

export const PALETTE_IDS: PaletteId[] = ["dark", "light", "ocean", "sage"];

export const V2_PALETTES: Record<PaletteId, V2Palette> = {
  dark: {
    id: "dark",
    name: "Dark",
    kind: "dark",
    summary: "Neutral dark surfaces with a muted slate-blue accent.",
    colors: withAliases(dark),
  },
  light: {
    id: "light",
    name: "Light",
    kind: "light",
    summary: "Neutral light surfaces, white documents, a muted slate-blue accent.",
    colors: withAliases(light),
  },
  ocean: {
    id: "ocean",
    name: "Ocean",
    kind: "dark",
    summary: "Gray-navy surfaces with a muted teal accent; focus and info stay blue.",
    colors: withAliases(ocean),
  },
  sage: {
    id: "sage",
    name: "Sage",
    kind: "light",
    summary: "Warm neutral light surfaces with a gray-green accent; focus and info stay blue.",
    colors: withAliases(sage),
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
