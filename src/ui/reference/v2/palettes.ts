// V2 review samples (phase 1): the four proposed palettes.
//
// Sample-only. Nothing here is applied to the application: production colors
// stay in `src/ui/tokens.css`. Each palette uses exactly the production color
// role names (`ColorRole`, `--color-<role>`), so a chosen palette maps 1:1 onto
// `tokens.css` later. Aliases follow the same derivations as `tokens.css`
// (status-running = info, idle/unavailable/canceled = neutral, status bar =
// surface-base + text-muted, graph edges = category borders).
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
  "surface-base": "#edeff2",
  "surface-sunken": "#f6f7f9",
  surface: "#ffffff",
  "surface-raised": "#ffffff",
  "surface-hover": "#eaecf0",
  "surface-active": "#e0e3e8",
  "surface-selected": "#e3e8f0",
  field: "#ffffff",
  overlay: "rgba(15, 18, 24, 0.28)",
  text: "#1d2128",
  "text-muted": "#4a515d",
  "text-subtle": "#5b6270",
  "text-disabled": "#9ea4ae",
  "text-on-accent": "#ffffff",
  border: "#e0e3e8",
  "border-subtle": "#eceef1",
  "border-strong": "#7a818e",
  accent: "#2b5bc4",
  "accent-hover": "#2450ad",
  "accent-active": "#1f4697",
  "accent-text": "#2553b6",
  "accent-subtle": "#e2eafa",
  "focus-ring": "#2a64e0",
  selection: "#cbdaf6",
  "selection-inactive": "#e3e6ec",
  "danger-text": "#b3261e",
  "danger-bg": "#fdeeec",
  "danger-border": "#f0bdb7",
  "danger-solid": "#c22b2b",
  "danger-solid-hover": "#a52323",
  "warning-text": "#8a4a00",
  "warning-bg": "#fdf3e2",
  "warning-border": "#eccf97",
  "success-text": "#1b6f3c",
  "success-bg": "#e8f4ec",
  "success-border": "#afd8bc",
  "info-text": "#0b6386",
  "info-bg": "#e5f2f8",
  "info-border": "#a9d3e6",
  "status-stale-text": "#6a43c4",
  "status-stale-bg": "#f2edfa",
  "status-stale-border": "#d6c8f1",
  "status-neutral-text": "#4a515d",
  "status-neutral-bg": "#eef0f3",
  "status-neutral-border": "#d7dbe1",
  "tooltip-bg": "#1d2128",
  "tooltip-text": "#f3f4f6",
  "kbd-bg": "#eef0f3",
  "kbd-border": "#cbd0d7",
  "kbd-text": "#4a515d",
  scrollbar: "rgba(91, 98, 112, 0.32)",
  "scrollbar-hover": "rgba(91, 98, 112, 0.52)",
  shadow: "rgba(16, 24, 40, 0.12)",
  "shadow-strong": "rgba(16, 24, 40, 0.22)",
  "graph-grid": "#d7dbe1",
  "graph-structure-bg": "#eef0f3",
  "graph-structure-border": "#6b7280",
  "graph-structure-text": "#4a515d",
  "graph-document-bg": "#e6eefc",
  "graph-document-border": "#3d6fd6",
  "graph-document-text": "#1f50b8",
  "graph-data-bg": "#e2f4ed",
  "graph-data-border": "#1d8a68",
  "graph-data-text": "#11624a",
  "graph-math-bg": "#f1ebf9",
  "graph-math-border": "#8259d4",
  "graph-math-text": "#5a37a6",
  "graph-formal-bg": "#fdf0dc",
  "graph-formal-border": "#bd6a09",
  "graph-formal-text": "#874700",
  "graph-reference-bg": "#fbe8f0",
  "graph-reference-border": "#c43e70",
  "graph-reference-text": "#9a2252",
  "graph-workflow-bg": "#e0f3f7",
  "graph-workflow-border": "#18849f",
  "graph-workflow-text": "#0d5e73",
  "graph-edge": "#737a86",
};

const dark: BaseColors = {
  "surface-base": "#151618",
  "surface-sunken": "#191a1d",
  surface: "#1e1f22",
  "surface-raised": "#27292d",
  "surface-hover": "#2a2c30",
  "surface-active": "#323539",
  "surface-selected": "#2c3036",
  field: "#222327",
  overlay: "rgba(0, 0, 0, 0.5)",
  text: "#e2e4e8",
  "text-muted": "#b0b4bc",
  "text-subtle": "#979ca5",
  "text-disabled": "#686d75",
  "text-on-accent": "#ffffff",
  border: "#2d2f33",
  "border-subtle": "#242629",
  "border-strong": "#7a7f88",
  accent: "#386fdb",
  "accent-hover": "#3365cb",
  "accent-active": "#2d5bb8",
  "accent-text": "#86adf2",
  "accent-subtle": "#1f2c45",
  "focus-ring": "#5b9bff",
  selection: "#284a72",
  "selection-inactive": "#34373c",
  "danger-text": "#f28b82",
  "danger-bg": "#3a1d1d",
  "danger-border": "#6b2f2c",
  "danger-solid": "#c22b2b",
  "danger-solid-hover": "#ae2424",
  "warning-text": "#e9b15c",
  "warning-bg": "#352812",
  "warning-border": "#66491d",
  "success-text": "#73cc8f",
  "success-bg": "#17301f",
  "success-border": "#2c5638",
  "info-text": "#6cc3e6",
  "info-bg": "#15303b",
  "info-border": "#285465",
  "status-stale-text": "#c0a5f5",
  "status-stale-bg": "#2b2440",
  "status-stale-border": "#4c3e70",
  "status-neutral-text": "#b0b4bc",
  "status-neutral-bg": "#292b2f",
  "status-neutral-border": "#3a3d42",
  "tooltip-bg": "#43464c",
  "tooltip-text": "#f3f4f6",
  "kbd-bg": "#33363b",
  "kbd-border": "#4a4d54",
  "kbd-text": "#c8ccd2",
  scrollbar: "rgba(150, 155, 164, 0.3)",
  "scrollbar-hover": "rgba(150, 155, 164, 0.5)",
  shadow: "rgba(0, 0, 0, 0.35)",
  "shadow-strong": "rgba(0, 0, 0, 0.55)",
  "graph-grid": "#2d2f33",
  "graph-structure-bg": "#2a2c30",
  "graph-structure-border": "#868d98",
  "graph-structure-text": "#c8ccd2",
  "graph-document-bg": "#1a2a45",
  "graph-document-border": "#5b8fea",
  "graph-document-text": "#9cc0ff",
  "graph-data-bg": "#13302a",
  "graph-data-border": "#3fb594",
  "graph-data-text": "#7fdcbf",
  "graph-math-bg": "#2c2342",
  "graph-math-border": "#9d7ae6",
  "graph-math-text": "#cdb5ff",
  "graph-formal-bg": "#3a2a12",
  "graph-formal-border": "#d98c2b",
  "graph-formal-text": "#f3bd6d",
  "graph-reference-bg": "#3a1b29",
  "graph-reference-border": "#dd6491",
  "graph-reference-text": "#f5a3c3",
  "graph-workflow-bg": "#12303a",
  "graph-workflow-border": "#39a9c7",
  "graph-workflow-text": "#8bd6ea",
  "graph-edge": "#8a909b",
};

const ocean: BaseColors = {
  "surface-base": "#0d1520",
  "surface-sunken": "#111a26",
  surface: "#15202d",
  "surface-raised": "#1c2838",
  "surface-hover": "#1e2b3b",
  "surface-active": "#263548",
  "surface-selected": "#213046",
  field: "#182333",
  overlay: "rgba(2, 6, 12, 0.55)",
  text: "#dde5ee",
  "text-muted": "#a8b5c5",
  "text-subtle": "#8f9daf",
  "text-disabled": "#5a687a",
  "text-on-accent": "#ffffff",
  border: "#223043",
  "border-subtle": "#1a2533",
  "border-strong": "#6c7e96",
  accent: "#20807a",
  "accent-hover": "#1c746f",
  "accent-active": "#196864",
  "accent-text": "#5ec8bd",
  "accent-subtle": "#15363b",
  "focus-ring": "#6aa9ff",
  selection: "#24466a",
  "selection-inactive": "#283649",
  "danger-text": "#ff8f8a",
  "danger-bg": "#3a1a22",
  "danger-border": "#6c2d36",
  "danger-solid": "#c22b33",
  "danger-solid-hover": "#ae262d",
  "warning-text": "#f0b65e",
  "warning-bg": "#33291a",
  "warning-border": "#66501f",
  "success-text": "#8ad69a",
  "success-bg": "#14302a",
  "success-border": "#2a5a43",
  "info-text": "#86b6ff",
  "info-bg": "#172b4a",
  "info-border": "#2e4c7c",
  "status-stale-text": "#c3a8ff",
  "status-stale-bg": "#272448",
  "status-stale-border": "#48407a",
  "status-neutral-text": "#a8b5c5",
  "status-neutral-bg": "#1f2b3b",
  "status-neutral-border": "#304055",
  "tooltip-bg": "#34445a",
  "tooltip-text": "#f1f5f9",
  "kbd-bg": "#263548",
  "kbd-border": "#3b4d66",
  "kbd-text": "#c3cedb",
  scrollbar: "rgba(141, 155, 173, 0.3)",
  "scrollbar-hover": "rgba(141, 155, 173, 0.5)",
  shadow: "rgba(0, 0, 0, 0.4)",
  "shadow-strong": "rgba(0, 0, 0, 0.6)",
  "graph-grid": "#223043",
  "graph-structure-bg": "#1f2b3b",
  "graph-structure-border": "#8796ab",
  "graph-structure-text": "#c3cedb",
  "graph-document-bg": "#172b4a",
  "graph-document-border": "#5b8fea",
  "graph-document-text": "#9cc0ff",
  "graph-data-bg": "#11302f",
  "graph-data-border": "#3fb594",
  "graph-data-text": "#7fdcbf",
  "graph-math-bg": "#262347",
  "graph-math-border": "#9d7ae6",
  "graph-math-text": "#cdb5ff",
  "graph-formal-bg": "#33291a",
  "graph-formal-border": "#d98c2b",
  "graph-formal-text": "#f3bd6d",
  "graph-reference-bg": "#36192c",
  "graph-reference-border": "#dd6491",
  "graph-reference-text": "#f5a3c3",
  "graph-workflow-bg": "#10303d",
  "graph-workflow-border": "#39a9c7",
  "graph-workflow-text": "#8bd6ea",
  "graph-edge": "#8796ab",
};

const sage: BaseColors = {
  "surface-base": "#eae9e3",
  "surface-sunken": "#f4f3ee",
  surface: "#fcfcfa",
  "surface-raised": "#ffffff",
  "surface-hover": "#e7e6df",
  "surface-active": "#dcdbd2",
  "surface-selected": "#e1e6dc",
  field: "#ffffff",
  overlay: "rgba(28, 30, 24, 0.28)",
  text: "#1f221d",
  "text-muted": "#4a4e45",
  "text-subtle": "#5a5e55",
  "text-disabled": "#a1a39a",
  "text-on-accent": "#ffffff",
  border: "#dddbd2",
  "border-subtle": "#e9e8e1",
  "border-strong": "#787b71",
  accent: "#4b6b58",
  "accent-hover": "#415f4d",
  "accent-active": "#385342",
  "accent-text": "#3d5f4b",
  "accent-subtle": "#e0e9dd",
  "focus-ring": "#2f63c9",
  selection: "#d3def0",
  "selection-inactive": "#e3e3dc",
  "danger-text": "#ad2a1f",
  "danger-bg": "#fbecea",
  "danger-border": "#edbdb6",
  "danger-solid": "#bf2d2a",
  "danger-solid-hover": "#a32522",
  "warning-text": "#844f08",
  "warning-bg": "#fbf1dc",
  "warning-border": "#e8cf98",
  "success-text": "#17703a",
  "success-bg": "#e4f2e6",
  "success-border": "#a9d4b3",
  "info-text": "#23589f",
  "info-bg": "#e6eef8",
  "info-border": "#b5c9e6",
  "status-stale-text": "#6943b8",
  "status-stale-bg": "#f0ebf7",
  "status-stale-border": "#d4c8ec",
  "status-neutral-text": "#4a4e45",
  "status-neutral-bg": "#ecebe5",
  "status-neutral-border": "#d6d5cc",
  "tooltip-bg": "#262924",
  "tooltip-text": "#f3f3ef",
  "kbd-bg": "#ecebe5",
  "kbd-border": "#cac9bf",
  "kbd-text": "#4a4e45",
  scrollbar: "rgba(90, 94, 85, 0.32)",
  "scrollbar-hover": "rgba(90, 94, 85, 0.52)",
  shadow: "rgba(30, 32, 25, 0.12)",
  "shadow-strong": "rgba(30, 32, 25, 0.22)",
  "graph-grid": "#d6d5cc",
  "graph-structure-bg": "#ecebe5",
  "graph-structure-border": "#6d7066",
  "graph-structure-text": "#4a4e45",
  "graph-document-bg": "#e6eefa",
  "graph-document-border": "#3d6fd6",
  "graph-document-text": "#1f50b8",
  "graph-data-bg": "#e0f1e9",
  "graph-data-border": "#1d8a68",
  "graph-data-text": "#11624a",
  "graph-math-bg": "#efeaf6",
  "graph-math-border": "#8259d4",
  "graph-math-text": "#5a37a6",
  "graph-formal-bg": "#f9eedb",
  "graph-formal-border": "#bd6a09",
  "graph-formal-text": "#874700",
  "graph-reference-bg": "#f8e7ec",
  "graph-reference-border": "#c43e70",
  "graph-reference-text": "#9a2252",
  "graph-workflow-bg": "#ddf0f2",
  "graph-workflow-border": "#18849f",
  "graph-workflow-text": "#0d5e73",
  "graph-edge": "#74776d",
};

export const PALETTE_IDS: PaletteId[] = ["dark", "light", "ocean", "sage"];

export const V2_PALETTES: Record<PaletteId, V2Palette> = {
  dark: {
    id: "dark",
    name: "Dark",
    kind: "dark",
    summary: "Quiet neutral dark surfaces, cobalt accent, readable muted text.",
    colors: withAliases(dark),
  },
  light: {
    id: "light",
    name: "Light",
    kind: "light",
    summary: "Clear neutral light surfaces with softer boundaries; white documents; cobalt accent.",
    colors: withAliases(light),
  },
  ocean: {
    id: "ocean",
    name: "Ocean",
    kind: "dark",
    summary: "Provisional. Dark navy surfaces with a restrained teal accent; focus and info stay blue.",
    colors: withAliases(ocean),
  },
  sage: {
    id: "sage",
    name: "Sage",
    kind: "light",
    summary: "Provisional. Warm neutral light surfaces with a gray-green accent; focus and info stay blue.",
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
