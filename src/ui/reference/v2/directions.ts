// V2 review samples: the three directions as geometry. Each direction is a set
// of local custom properties on the sample frame (plus a `data-direction`
// attribute for helper-text placement); colors come from one shared palette.
// Values are pixels unless noted.
import type { CSSProperties } from "react";

export type DirectionId = "compact" | "balanced" | "reading";
export const DIRECTION_IDS: DirectionId[] = ["compact", "balanced", "reading"];

export interface Geometry {
  /** Ordinary UI text and controls. */
  font: number;
  /** Meta, helper text, status bar. */
  fontSm: number;
  /** Agent prose, settings descriptions, dialog text. */
  fontProse: number;
  lineProse: number;
  /** Page and inspector titles. */
  fontTitle: number;
  /** Tree and list rows. */
  row: number;
  /** Menu rows. */
  rowMenu: number;
  /** Buttons and fields (md), small buttons (sm). */
  control: number;
  controlSm: number;
  tab: number;
  paneHeader: number;
  pageBar: number;
  statusBar: number;
  sidebar: number;
  /** Horizontal pane inset. */
  inset: number;
  /** Between related items (label and control, buttons in a row). */
  gapRelated: number;
  /** Between groups (settings rows, agent blocks). */
  gapGroup: number;
  /** Between sections (settings sections, conversation turns). */
  gapSection: number;
  radius: number;
  radiusContainer: number;
  /** Padding inside agent blocks and banners. */
  pad: number;
  codeLine: number;
}

export interface Direction {
  id: DirectionId;
  name: string;
  summary: string;
  geometry: Geometry;
  /** Where helper text goes (for the review notes). */
  helper: string;
  boundaries: string;
}

export const DIRECTIONS: Record<DirectionId, Direction> = {
  compact: {
    id: "compact",
    name: "Compact Calm",
    summary: "Familiar compact proportions, quieter boundaries, less repeated text.",
    helper: "Settings: label left, control right; a needed hint sits under the control.",
    boundaries: "Faint dividers only between panes; headers separate by tone, not lines.",
    geometry: {
      font: 13,
      fontSm: 12,
      fontProse: 13,
      lineProse: 1.45,
      fontTitle: 14,
      row: 24,
      rowMenu: 26,
      control: 26,
      controlSm: 22,
      tab: 32,
      paneHeader: 32,
      pageBar: 32,
      statusBar: 22,
      sidebar: 252,
      inset: 10,
      gapRelated: 6,
      gapGroup: 12,
      gapSection: 20,
      radius: 4,
      radiusContainer: 5,
      pad: 8,
      codeLine: 19,
    },
  },
  balanced: {
    id: "balanced",
    name: "Balanced Workspace",
    summary: "Somewhat more separation between groups and a clearer reading hierarchy.",
    helper: "Settings: label with its hint stacked on the left, control on the right; sections titled with a rule.",
    boundaries: "Standard dividers; section titles carry a rule; cards stay unbordered insets.",
    geometry: {
      font: 13,
      fontSm: 12,
      fontProse: 14,
      lineProse: 1.5,
      fontTitle: 15,
      row: 26,
      rowMenu: 28,
      control: 28,
      controlSm: 24,
      tab: 35,
      paneHeader: 35,
      pageBar: 34,
      statusBar: 24,
      sidebar: 264,
      inset: 14,
      gapRelated: 8,
      gapGroup: 18,
      gapSection: 32,
      radius: 5,
      radiusContainer: 6,
      pad: 12,
      codeLine: 20,
    },
  },
  reading: {
    id: "reading",
    name: "Reading Ease",
    summary: "Slightly more generous prose and form spacing; navigation stays efficient.",
    helper: "Settings: one column, label above the control and the hint between them; width capped for reading.",
    boundaries: "Few rules; whitespace groups sections; navigation rows keep compact height.",
    geometry: {
      font: 13,
      fontSm: 12,
      fontProse: 15,
      lineProse: 1.6,
      fontTitle: 16,
      row: 26,
      rowMenu: 30,
      control: 32,
      controlSm: 26,
      tab: 36,
      paneHeader: 36,
      pageBar: 34,
      statusBar: 24,
      sidebar: 272,
      inset: 16,
      gapRelated: 10,
      gapGroup: 22,
      gapSection: 40,
      radius: 6,
      radiusContainer: 8,
      pad: 14,
      codeLine: 21,
    },
  },
};

const VAR_NAMES: Record<keyof Geometry, string> = {
  font: "--v2-font",
  fontSm: "--v2-font-sm",
  fontProse: "--v2-font-prose",
  lineProse: "--v2-line-prose",
  fontTitle: "--v2-font-title",
  row: "--v2-row",
  rowMenu: "--v2-row-menu",
  control: "--v2-control",
  controlSm: "--v2-control-sm",
  tab: "--v2-tab",
  paneHeader: "--v2-pane-header",
  pageBar: "--v2-page-bar",
  statusBar: "--v2-status-bar",
  sidebar: "--v2-sidebar",
  inset: "--v2-inset",
  gapRelated: "--v2-gap-related",
  gapGroup: "--v2-gap-group",
  gapSection: "--v2-gap-section",
  radius: "--v2-radius",
  radiusContainer: "--v2-radius-container",
  pad: "--v2-pad",
  codeLine: "--v2-code-line",
};

/** The local custom properties for a direction's frame. */
export function geometryStyle(g: Geometry): CSSProperties {
  const style: Record<string, string> = {};
  for (const [key, name] of Object.entries(VAR_NAMES) as [keyof Geometry, string][]) {
    style[name] = key === "lineProse" ? String(g[key]) : `${g[key]}px`;
  }
  return style as CSSProperties;
}
