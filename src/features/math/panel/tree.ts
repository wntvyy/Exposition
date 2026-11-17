// Pure helpers over the editing tree for the panel's rendering and keyboard
// handling, following the core's slot-path convention (`SlotPath` in
// core/types.ts): alternating node index and slot name; field names for
// slots, "r<row>c<col>" for grid cells, and a trailing [index, "text"] for a
// caret inside a text node.
import type { CatalogEntry, MathLanguage, MathNode, MathRow, SlotPath } from "../core";

/** One editable child row of a node, with the slot name used in caret paths. */
export interface Slot {
  name: string;
  row: MathRow;
  /** Short accessible description ("numerator", "row 1, column 2"). */
  label: string;
}

/** Name of a grid cell's slot (matrix, cases, aligned): "r0c1" (0-based). */
export const gridSlot = (row: number, col: number): string => `r${row}c${col}`;

/** The slot name of a caret inside a text node (path ends with [index, "text"]). */
export const TEXT_SLOT = "text";

/** The caret is inside the text node at `index` of the row at `rowPath`. */
export const isTextCaret = (caretPath: SlotPath, rowPath: SlotPath, index: number): boolean =>
  caretPath.length === rowPath.length + 2 &&
  caretPath[caretPath.length - 1] === TEXT_SLOT &&
  caretPath[caretPath.length - 2] === index &&
  rowPath.every((p, i) => caretPath[i] === p);

/** The editable child rows of a node, in reading order. */
export function slotsOf(node: MathNode): Slot[] {
  switch (node.kind) {
    case "frac":
      return [
        { name: "num", row: node.num, label: "numerator" },
        { name: "den", row: node.den, label: "denominator" },
      ];
    case "sqrt":
      return [
        ...(node.index ? [{ name: "index", row: node.index, label: "root index" }] : []),
        { name: "body", row: node.body, label: "radicand" },
      ];
    case "scripts":
      return [
        { name: "base", row: node.base, label: "base" },
        ...(node.sub ? [{ name: "sub", row: node.sub, label: "subscript" }] : []),
        ...(node.sup ? [{ name: "sup", row: node.sup, label: "superscript" }] : []),
      ];
    case "bigop":
      return [
        ...(node.lower ? [{ name: "lower", row: node.lower, label: "lower bound" }] : []),
        ...(node.upper ? [{ name: "upper", row: node.upper, label: "upper bound" }] : []),
      ];
    case "delim":
    case "accent":
    case "font":
      return [{ name: "body", row: node.body, label: "body" }];
    case "matrix":
    case "aligned":
      return node.rows.flatMap((cells, r) =>
        cells.map((row, c) => ({ name: gridSlot(r, c), row, label: `row ${r + 1}, column ${c + 1}` })),
      );
    case "cases":
      return node.rows.flatMap(([value, condition], r) => [
        { name: gridSlot(r, 0), row: value, label: `case ${r + 1} value` },
        { name: gridSlot(r, 1), row: condition, label: `case ${r + 1} condition` },
      ]);
    default:
      return [];
  }
}

/** Path of a child slot of the node at `index` in the row at `rowPath`. */
export const childPath = (rowPath: SlotPath, index: number, slot: string): SlotPath => [...rowPath, index, slot];

export const samePath = (a: SlotPath, b: SlotPath): boolean => a.length === b.length && a.every((p, i) => p === b[i]);

/** The nodes along a caret path, outermost first (the structures the caret is inside). */
export function nodesOnPath(root: MathRow, path: SlotPath): MathNode[] {
  const nodes: MathNode[] = [];
  let row: MathRow | undefined = root;
  for (let i = 0; i + 1 < path.length && row; i += 2) {
    const node: MathNode | undefined = row.children[Number(path[i])];
    if (!node) break;
    nodes.push(node);
    row = slotsOf(node).find((s) => s.name === path[i + 1])?.row;
  }
  return nodes;
}

/** The caret is inside a matrix, cases, or aligned cell (Enter there is ambiguous). */
export function inGrid(root: MathRow, path: SlotPath): boolean {
  return nodesOnPath(root, path).some((n) => n.kind === "matrix" || n.kind === "cases" || n.kind === "aligned");
}

/** Every opaque segment in the tree (source the adapter did not understand). */
export function opaqueNodes(row: MathRow): Extract<MathNode, { kind: "opaque" }>[] {
  const found: Extract<MathNode, { kind: "opaque" }>[] = [];
  const visit = (r: MathRow) => {
    for (const node of r.children) {
      if (node.kind === "opaque") found.push(node);
      for (const slot of slotsOf(node)) visit(slot.row);
    }
  };
  visit(row);
  return found;
}

export const isEmptyRow = (row: MathRow): boolean => row.children.length === 0;

export const LANGUAGE_LABELS: Record<MathLanguage, string> = { latex: "LaTeX", typst: "Typst", markdown: "Markdown" };

/** Whether a catalog entry can be written in `language` (browse view; search reports it itself). */
export function availableIn(entry: CatalogEntry, language: MathLanguage): boolean {
  switch (language) {
    case "latex":
      return entry.latex !== null;
    case "typst":
      return entry.typst !== null;
    case "markdown":
      return entry.markdown;
  }
}

/** The command a user would recognize for an entry in `language` ("\\leq", "lt.eq"). */
export function commandFor(entry: CatalogEntry, language: MathLanguage): string | null {
  if (language === "typst") return entry.typst?.name ?? null;
  // Markdown math is TeX-like.
  return entry.latex?.command ?? null;
}
