// Pure helpers over the math tree: construction, slot access, and equality.
import type { MathNode, MathRow } from "./types";

export const row = (children: MathNode[] = []): MathRow => ({ kind: "row", children });

export const isEmptyRow = (r: MathRow | null | undefined): boolean => !r || r.children.length === 0;

/** Name of a grid cell slot ("r0c1"). */
export const cellName = (r: number, c: number): string => `r${r}c${c}`;

/** Parse a grid cell slot name. */
export function parseCell(name: string | number | undefined): { r: number; c: number } | null {
  if (typeof name !== "string") return null;
  const m = /^r(\d+)c(\d+)$/.exec(name);
  return m ? { r: Number(m[1]), c: Number(m[2]) } : null;
}

/** The present (non-null) slots of a node, in caret order. */
export function slotNames(node: MathNode): string[] {
  switch (node.kind) {
    case "frac":
      return ["num", "den"];
    case "sqrt":
      return node.index ? ["index", "body"] : ["body"];
    case "scripts":
      return ["base", ...(node.sub ? ["sub"] : []), ...(node.sup ? ["sup"] : [])];
    case "bigop":
      return [...(node.lower ? ["lower"] : []), ...(node.upper ? ["upper"] : [])];
    case "delim":
    case "accent":
    case "font":
      return ["body"];
    case "matrix":
    case "aligned":
      return node.rows.flatMap((cells, r) => cells.map((_, c) => cellName(r, c)));
    case "cases":
      return node.rows.flatMap((_, r) => [cellName(r, 0), cellName(r, 1)]);
    default:
      return [];
  }
}

export function getSlot(node: MathNode, name: string | number | undefined): MathRow | null {
  if (typeof name !== "string") return null;
  const cell = parseCell(name);
  switch (node.kind) {
    case "frac":
      return name === "num" ? node.num : name === "den" ? node.den : null;
    case "sqrt":
      return name === "body" ? node.body : name === "index" ? node.index : null;
    case "scripts":
      return name === "base" ? node.base : name === "sub" ? node.sub : name === "sup" ? node.sup : null;
    case "bigop":
      return name === "lower" ? node.lower : name === "upper" ? node.upper : null;
    case "delim":
    case "accent":
    case "font":
      return name === "body" ? node.body : null;
    case "matrix":
    case "aligned":
      return cell ? (node.rows[cell.r]?.[cell.c] ?? null) : null;
    case "cases":
      return cell && cell.c < 2 ? (node.rows[cell.r]?.[cell.c] ?? null) : null;
    default:
      return null;
  }
}

/** A copy of `node` with slot `name` replaced (null removes an optional slot). */
export function setSlot(node: MathNode, name: string, value: MathRow | null): MathNode {
  const cell = parseCell(name);
  switch (node.kind) {
    case "frac":
      return name === "num" ? { ...node, num: value ?? row() } : { ...node, den: value ?? row() };
    case "sqrt":
      return name === "index" ? { ...node, index: value } : { ...node, body: value ?? row() };
    case "scripts":
      if (name === "base") return { ...node, base: value ?? row() };
      return name === "sub" ? { ...node, sub: value } : { ...node, sup: value };
    case "bigop":
      return name === "lower" ? { ...node, lower: value } : { ...node, upper: value };
    case "delim":
    case "accent":
    case "font":
      return { ...node, body: value ?? row() };
    case "matrix":
    case "aligned": {
      if (!cell) return node;
      const rows = node.rows.map((cells, r) => (r === cell.r ? cells.map((c, i) => (i === cell.c ? (value ?? row()) : c)) : cells));
      return { ...node, rows };
    }
    case "cases": {
      if (!cell) return node;
      const rows = node.rows.map((pair, r): [MathRow, MathRow] =>
        r === cell.r ? (cell.c === 0 ? [value ?? row(), pair[1]] : [pair[0], value ?? row()]) : pair,
      );
      return { ...node, rows };
    }
    default:
      return node;
  }
}

/** Nodes that own editable slots. */
export const hasSlots = (node: MathNode): boolean => slotNames(node).length > 0;

/** Structural equality, treating absent and undefined fields alike. */
export function sameValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) {
    const bb = b as unknown[];
    return a.length === bb.length && a.every((x, i) => sameValue(x, bb[i]));
  }
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const keys = new Set([...Object.keys(ao), ...Object.keys(bo)]);
  for (const k of keys) if (!sameValue(ao[k], bo[k])) return false;
  return true;
}

export function sameMath(a: MathRow, b: MathRow): boolean {
  return sameValue(a, b);
}

/** Visit every node of a row recursively. */
export function walk(r: MathRow, visit: (node: MathNode) => void): void {
  for (const node of r.children) {
    visit(node);
    if (node.kind === "cases") {
      for (const [a, b] of node.rows) {
        walk(a, visit);
        walk(b, visit);
      }
      continue;
    }
    for (const name of slotNames(node)) {
      const slot = getSlot(node, name);
      if (slot) walk(slot, visit);
    }
  }
}

/** True when the row contains no opaque node at any depth. */
export function isComplete(r: MathRow): boolean {
  let ok = true;
  walk(r, (n) => {
    if (n.kind === "opaque") ok = false;
  });
  return ok;
}
