// Structural editing model over the math tree. Every operation is pure and
// returns a new state.
//
// Rules (stable, relied on by the panel):
// - Caret: a row path plus an offset between children. Left/right walk into a
//   structure's first/last slot and out of it after its last/before its first
//   slot; up/down move between numerator/denominator, superscript/subscript,
//   upper/lower limits, root index/body, and grid rows; nextSlot/prevSlot (Tab /
//   Shift-Tab) step through every slot in document order (pre-order).
// - Typing: letters are identifiers, digits numbers; `^`/`_` put scripts on the
//   previous atom (or limits on an operator); `/` makes a fraction with the
//   previous atom (a digit run counts as one) or the selection as numerator;
//   `(`, `[`, `{` open a fixed-size pair around the selection; typing the
//   closing character at the end of such a pair steps out of it.
// - Templates wrap a non-empty selection into their first structurally valid
//   slot and put the caret in the first empty slot (or the end of the first).
// - Backspace/Delete next to an atom removes it. Next to a structure, text, or
//   opaque source that has content it first selects the whole node; a second
//   press deletes the selection. Empty structures are removed at once.
//   Backspace at the start of a slot: an empty optional slot (script, limit,
//   root index) is removed; the first slot of a single-body structure (root,
//   delimiters, accent, font) or of a fraction unwraps it (the contents stay in
//   place); otherwise the caret moves to the end of the previous slot or out
//   before the structure. Delete at the end of a slot mirrors this.
import { classOf, entryByGlyph, entryById } from "./catalog";
import { isLimitFunc, serializeLatexMath } from "./latex";
import { cellName, getSlot, hasSlots, isEmptyRow, parseCell, row, sameMath, setSlot, slotNames } from "./tree";
import type { EditorState, MathCaret, MathNode, MathRow, SlotPath, SymbolId, TemplateId } from "./types";

export type Motion = "left" | "right" | "up" | "down" | "nextSlot" | "prevSlot" | "home" | "end";
export type GridEdit = "addRowAfter" | "addRowBefore" | "addColAfter" | "addColBefore" | "removeRow" | "removeCol";

interface Step {
  index: number;
  slot: string;
}

// ================================================================ paths

function toSteps(path: SlotPath): Step[] {
  const out: Step[] = [];
  for (let i = 0; i + 1 < path.length; i += 2) out.push({ index: path[i] as number, slot: String(path[i + 1]) });
  return out;
}

/** A caret inside a text node: the path ends with [nodeIndex, "text"]. */
const isTextPath = (path: SlotPath) => path.length >= 2 && path[path.length - 1] === "text";
/** Path of the row that contains the caret (for a text caret, the row holding the text node). */
const rowPathOf = (path: SlotPath) => (isTextPath(path) ? path.slice(0, -2) : path);

function rowAt(root: MathRow, path: SlotPath): MathRow | null {
  let r: MathRow | null = root;
  for (const s of toSteps(path)) {
    const node: MathNode | undefined = r?.children[s.index];
    if (!node) return null;
    r = getSlot(node, s.slot);
    if (!r) return null;
  }
  return r;
}

function updateRow(root: MathRow, steps: Step[], f: (r: MathRow) => MathRow): MathRow {
  if (steps.length === 0) return f(root);
  const [first, ...rest] = steps as [Step, ...Step[]];
  const node = root.children[first.index];
  if (!node) return root;
  const slot = getSlot(node, first.slot);
  if (!slot) return root;
  const updated = setSlot(node, first.slot, updateRow(slot, rest, f));
  return row(root.children.map((n, i) => (i === first.index ? updated : n)));
}

const updateAt = (root: MathRow, path: SlotPath, f: (r: MathRow) => MathRow) => updateRow(root, toSteps(path), f);

function textNodeAt(root: MathRow, path: SlotPath): Extract<MathNode, { kind: "text" }> | null {
  if (!isTextPath(path)) return null;
  const r = rowAt(root, path.slice(0, -2));
  const n = r?.children[path[path.length - 2] as number];
  return n?.kind === "text" ? n : null;
}

const caretAt = (path: SlotPath, offset: number): MathCaret => ({ path, offset });

// ================================================================ state

export function createState(root?: MathRow): EditorState {
  const r = root ?? row();
  return { root: r, caret: caretAt([], r.children.length) };
}

export function setCaret(state: EditorState, caret: MathCaret): EditorState {
  const text = textNodeAt(state.root, caret.path);
  if (text) {
    const offset = clamp(caret.offset, 0, text.text.length);
    const anchor = caret.anchor === undefined ? undefined : clamp(caret.anchor, 0, text.text.length);
    return { root: state.root, caret: anchor === undefined || anchor === offset ? caretAt(caret.path, offset) : { path: caret.path, offset, anchor } };
  }
  const r = isTextPath(caret.path) ? null : rowAt(state.root, caret.path);
  if (!r) return { root: state.root, caret: caretAt([], state.root.children.length) };
  const offset = clamp(caret.offset, 0, r.children.length);
  const anchor = caret.anchor === undefined ? undefined : clamp(caret.anchor, 0, r.children.length);
  return { root: state.root, caret: anchor === undefined || anchor === offset ? caretAt(caret.path, offset) : { path: caret.path, offset, anchor } };
}

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

export { sameMath };

export function previewLatex(r: MathRow): string {
  return serializeLatexMath(r, "preview").source;
}

// ================================================================ motion

function enter(node: MathNode, rowPath: SlotPath, index: number, from: "left" | "right"): MathCaret | null {
  if (node.kind === "text") return caretAt([...rowPath, index, "text"], from === "left" ? 0 : node.text.length);
  const slots = slotNames(node);
  if (slots.length === 0) return null;
  const name = (from === "left" ? slots[0] : slots[slots.length - 1]) as string;
  const slot = getSlot(node, name) as MathRow;
  return caretAt([...rowPath, index, name], from === "left" ? 0 : slot.children.length);
}

/** Leave the current slot towards `dir`: the neighbouring slot of the same structure, or out of it. */
function exitSlot(root: MathRow, path: SlotPath, dir: "left" | "right"): MathCaret | null {
  if (path.length === 0) return null;
  const parentPath = path.slice(0, -2);
  const index = path[path.length - 2] as number;
  const slot = String(path[path.length - 1]);
  const parent = rowAt(root, parentPath) as MathRow;
  const node = parent.children[index] as MathNode;
  if (node.kind === "text") return caretAt(parentPath, dir === "left" ? index : index + 1);
  const slots = slotNames(node);
  const i = slots.indexOf(slot);
  const j = dir === "right" ? i + 1 : i - 1;
  if (i >= 0 && j >= 0 && j < slots.length) {
    const name = slots[j] as string;
    const target = getSlot(node, name) as MathRow;
    return caretAt([...parentPath, index, name], dir === "right" ? 0 : target.children.length);
  }
  return caretAt(parentPath, dir === "right" ? index + 1 : index);
}

function horizontal(state: EditorState, dir: "left" | "right"): MathCaret {
  const { root, caret } = state;
  const text = textNodeAt(root, caret.path);
  if (text) {
    if (dir === "right" && caret.offset < text.text.length) return caretAt(caret.path, caret.offset + 1);
    if (dir === "left" && caret.offset > 0) return caretAt(caret.path, caret.offset - 1);
    return exitSlot(root, caret.path, dir) ?? caret;
  }
  const r = rowAt(root, caret.path) as MathRow;
  if (dir === "right" && caret.offset < r.children.length) {
    const node = r.children[caret.offset] as MathNode;
    return enter(node, caret.path, caret.offset, "left") ?? caretAt(caret.path, caret.offset + 1);
  }
  if (dir === "left" && caret.offset > 0) {
    const node = r.children[caret.offset - 1] as MathNode;
    return enter(node, caret.path, caret.offset - 1, "right") ?? caretAt(caret.path, caret.offset - 1);
  }
  return exitSlot(root, caret.path, dir) ?? caretAt(caret.path, caret.offset);
}

function verticalTarget(node: MathNode, slot: string, dir: "up" | "down"): string | null {
  const up = dir === "up";
  switch (node.kind) {
    case "frac":
      return slot === "num" && !up ? "den" : slot === "den" && up ? "num" : null;
    case "scripts":
      if (slot === "base") return up ? (node.sup ? "sup" : null) : node.sub ? "sub" : null;
      if (slot === "sup" && !up) return node.sub ? "sub" : null;
      if (slot === "sub" && up) return node.sup ? "sup" : null;
      return null;
    case "bigop":
      return slot === "upper" && !up ? (node.lower ? "lower" : null) : slot === "lower" && up ? (node.upper ? "upper" : null) : null;
    case "sqrt":
      return slot === "body" && up ? (node.index ? "index" : null) : slot === "index" && !up ? "body" : null;
    case "matrix":
    case "aligned":
    case "cases": {
      const cell = parseCell(slot);
      if (!cell) return null;
      const r = cell.r + (up ? -1 : 1);
      const rows = node.rows as MathRow[][];
      if (r < 0 || r >= rows.length) return null;
      const width = node.kind === "cases" ? 2 : (rows[r] as MathRow[]).length;
      if (width === 0) return null;
      return cellName(r, Math.min(cell.c, width - 1));
    }
    default:
      return null;
  }
}

function vertical(state: EditorState, dir: "up" | "down"): MathCaret {
  const { root, caret } = state;
  const path = rowPathOf(caret.path);
  for (let depth = path.length; depth >= 2; depth -= 2) {
    const parentPath = path.slice(0, depth - 2);
    const index = path[depth - 2] as number;
    const slot = String(path[depth - 1]);
    const node = (rowAt(root, parentPath) as MathRow).children[index] as MathNode;
    const target = verticalTarget(node, slot, dir);
    if (target) {
      const t = getSlot(node, target) as MathRow;
      const offset = depth === path.length && !isTextPath(caret.path) ? Math.min(caret.offset, t.children.length) : t.children.length;
      return caretAt([...parentPath, index, target], offset);
    }
  }
  return caret;
}

/** Every row path in document order (pre-order, slots in caret order). */
function allRows(r: MathRow, path: SlotPath = [], out: SlotPath[] = []): SlotPath[] {
  out.push(path);
  r.children.forEach((node, i) => {
    for (const name of slotNames(node)) allRows(getSlot(node, name) as MathRow, [...path, i, name], out);
  });
  return out;
}

function slotStep(state: EditorState, dir: 1 | -1): MathCaret {
  const { root, caret } = state;
  const current = rowPathOf(caret.path);
  const rows = allRows(root);
  const key = JSON.stringify(current);
  const end = (p: SlotPath) => caretAt(p, (rowAt(root, p) as MathRow).children.length);
  if (current.length === 0) {
    // From the equation's own row: the first slot after (last slot before) the caret.
    const at = caret.offset;
    const slots = rows.slice(1);
    const target = dir === 1 ? slots.find((p) => (p[0] as number) >= at) : [...slots].reverse().find((p) => (p[0] as number) < at);
    return target ? end(target) : caretAt([], dir === 1 ? root.children.length : 0);
  }
  const i = Math.max(0, rows.findIndex((p) => JSON.stringify(p) === key));
  const j = i + dir;
  // rows[0] is the root itself: Tab only stops in slots.
  if (j >= 1 && j < rows.length) return end(rows[j] as SlotPath);
  // Past the last (before the first) slot: after (before) the outermost structure.
  return caretAt([], (current[0] as number) + (dir === 1 ? 1 : 0));
}

export function move(state: EditorState, motion: Motion, extend?: boolean): EditorState {
  const { root, caret } = state;
  const text = textNodeAt(root, caret.path);
  const len = text ? text.text.length : (rowAt(root, caret.path)?.children.length ?? 0);
  const selected = caret.anchor !== undefined && caret.anchor !== caret.offset;
  if (extend && (motion === "left" || motion === "right" || motion === "home" || motion === "end")) {
    const anchor = caret.anchor ?? caret.offset;
    const offset = motion === "home" ? 0 : motion === "end" ? len : clamp(caret.offset + (motion === "right" ? 1 : -1), 0, len);
    return { root, caret: anchor === offset ? caretAt(caret.path, offset) : { path: caret.path, offset, anchor } };
  }
  if (selected && (motion === "left" || motion === "right")) {
    const a = caret.anchor as number;
    return { root, caret: caretAt(caret.path, motion === "left" ? Math.min(a, caret.offset) : Math.max(a, caret.offset)) };
  }
  switch (motion) {
    case "left":
    case "right":
      return { root, caret: horizontal({ root, caret: caretAt(caret.path, caret.offset) }, motion) };
    case "up":
    case "down":
      return { root, caret: vertical(state, motion) };
    case "home":
      return { root, caret: caretAt(caret.path, 0) };
    case "end":
      return { root, caret: caretAt(caret.path, len) };
    case "nextSlot":
      return { root, caret: slotStep(state, 1) };
    case "prevSlot":
      return { root, caret: slotStep(state, -1) };
  }
}

// ================================================================ editing primitives

/** Remove the selection (if any) from the caret row; returns the removed nodes and the collapsed state. */
function takeSelection(state: EditorState): { state: EditorState; nodes: MathNode[] } {
  const { root, caret } = state;
  if (caret.anchor === undefined || caret.anchor === caret.offset || isTextPath(caret.path)) return { state, nodes: [] };
  const lo = Math.min(caret.anchor, caret.offset);
  const hi = Math.max(caret.anchor, caret.offset);
  const r = rowAt(root, caret.path) as MathRow;
  const nodes = r.children.slice(lo, hi);
  const newRoot = updateAt(root, caret.path, (x) => row([...x.children.slice(0, lo), ...x.children.slice(hi)]));
  return { state: { root: newRoot, caret: caretAt(caret.path, lo) }, nodes };
}

/** Insert nodes at the caret (replacing a selection); the caret goes after them. */
function insertNodes(state: EditorState, nodes: MathNode[]): EditorState {
  const s = takeSelection(state).state;
  const { caret } = s;
  const root = updateAt(s.root, caret.path, (x) => row([...x.children.slice(0, caret.offset), ...nodes, ...x.children.slice(caret.offset)]));
  return { root, caret: caretAt(caret.path, caret.offset + nodes.length) };
}

/** Insert a structure at the caret and put the caret into `slot` (at its end). */
function insertStructure(state: EditorState, node: MathNode, slot: string | null, extraAfter: MathNode[] = []): EditorState {
  const { caret } = state;
  const root = updateAt(state.root, caret.path, (x) => row([...x.children.slice(0, caret.offset), node, ...extraAfter, ...x.children.slice(caret.offset)]));
  if (slot === null) return { root, caret: caretAt(caret.path, caret.offset + 1) };
  if (slot === "text") return { root, caret: caretAt([...caret.path, caret.offset, "text"], 0) };
  const target = getSlot(node, slot) as MathRow;
  return { root, caret: caretAt([...caret.path, caret.offset, slot], target.children.length) };
}

/** First empty slot of a node, or its first slot. */
function firstEmptySlot(node: MathNode): string | null {
  const slots = slotNames(node);
  return slots.find((s) => isEmptyRow(getSlot(node, s))) ?? slots[0] ?? null;
}

const isOperatorLike = (n: MathNode | undefined) =>
  n?.kind === "symbol" && ["bin", "rel", "punct", "open"].includes(classOf(n.id));

/** Replace the node before the caret. */
function replacePrev(state: EditorState, node: MathNode, slot: string | null): EditorState {
  const { caret } = state;
  const at = caret.offset - 1;
  const root = updateAt(state.root, caret.path, (x) => row(x.children.map((n, i) => (i === at ? node : n))));
  if (slot === null) return { root, caret: caretAt(caret.path, caret.offset) };
  const target = getSlot(node, slot) as MathRow;
  return { root, caret: caretAt([...caret.path, at, slot], target.children.length) };
}

function script(state: EditorState, which: "sup" | "sub"): EditorState {
  const sel = takeSelection(state);
  if (sel.nodes.length > 0) {
    const node: MathNode = { kind: "scripts", base: row(sel.nodes), sub: null, sup: null, [which]: row() };
    return insertStructure(sel.state, node, which);
  }
  const { root, caret } = state;
  const r = rowAt(root, caret.path) as MathRow;
  // LaTeX habit: `x_i^2` — at the end of a script slot, the other script goes on the same base.
  if (caret.path.length >= 2 && caret.offset === r.children.length) {
    const slot = caret.path[caret.path.length - 1];
    const parentPath = caret.path.slice(0, -2);
    const index = caret.path[caret.path.length - 2] as number;
    const owner = (rowAt(root, parentPath) as MathRow).children[index];
    const other = which === "sup" ? "sub" : "sup";
    const limitOther = which === "sup" ? "lower" : "upper";
    const big = which === "sup" ? "upper" : "lower";
    if (owner?.kind === "scripts" && slot === other && owner[which] === null) {
      const updated: MathNode = { ...owner, [which]: row() };
      return { root: updateAt(root, parentPath, (x) => row(x.children.map((n, i) => (i === index ? updated : n)))), caret: caretAt([...parentPath, index, which], 0) };
    }
    if (owner?.kind === "bigop" && slot === limitOther && owner[big] === null) {
      const updated: MathNode = { ...owner, [big]: row() };
      return { root: updateAt(root, parentPath, (x) => row(x.children.map((n, i) => (i === index ? updated : n)))), caret: caretAt([...parentPath, index, big], 0) };
    }
  }
  const prev = caret.offset > 0 ? r.children[caret.offset - 1] : undefined;
  const big = which === "sup" ? "upper" : "lower";
  if (prev?.kind === "bigop") return replacePrev(state, { ...prev, [big]: prev[big] ?? row() }, big);
  if (prev?.kind === "func" && isLimitFunc(prev.name) && prev.text === undefined) {
    return replacePrev(state, { kind: "bigop", op: prev.name, lower: null, upper: null, [big]: row() }, big);
  }
  if (prev?.kind === "scripts") return replacePrev(state, { ...prev, [which]: prev[which] ?? row() }, which);
  if (prev && !isOperatorLike(prev)) {
    return replacePrev(state, { kind: "scripts", base: row([prev]), sub: null, sup: null, [which]: row() }, which);
  }
  return insertStructure(state, { kind: "scripts", base: row(), sub: null, sup: null, [which]: row() }, which);
}

function fraction(state: EditorState): EditorState {
  const sel = takeSelection(state);
  if (sel.nodes.length > 0) return insertStructure(sel.state, { kind: "frac", num: row(sel.nodes), den: row() }, "den");
  const { root, caret } = state;
  const r = rowAt(root, caret.path) as MathRow;
  let start = caret.offset;
  const prev = r.children[start - 1];
  if (prev && !isOperatorLike(prev)) {
    start--;
    if (prev.kind === "number") while (start > 0 && r.children[start - 1]?.kind === "number") start--;
  }
  if (start === caret.offset) return insertStructure(state, { kind: "frac", num: row(), den: row() }, "num");
  const num = r.children.slice(start, caret.offset);
  const node: MathNode = { kind: "frac", num: row(num), den: row() };
  const newRoot = updateAt(root, caret.path, (x) => row([...x.children.slice(0, start), node, ...x.children.slice(caret.offset)]));
  return { root: newRoot, caret: caretAt([...caret.path, start, "den"], 0) };
}

const OPEN_CHARS: Record<string, [SymbolId, SymbolId]> = {
  "(": ["delim.paren.l", "delim.paren.r"],
  "[": ["delim.bracket.l", "delim.bracket.r"],
  "{": ["delim.brace.l", "delim.brace.r"],
};
const CLOSE_CHARS: Record<string, SymbolId> = { ")": "delim.paren.r", "]": "delim.bracket.r", "}": "delim.brace.r" };
const CHAR_SYMBOLS: Record<string, SymbolId> = {
  "+": "op.plus",
  "-": "op.minus",
  "*": "op.asterisk",
  "/": "op.slash",
  "=": "rel.eq",
  "<": "rel.lt",
  ">": "rel.gt",
  ",": "punct.comma",
  ";": "punct.semicolon",
  ":": "punct.colon",
  "!": "punct.excl",
  "?": "punct.quest",
  "'": "calc.prime",
  "|": "delim.vert",
  ")": "delim.paren.r",
  "]": "delim.bracket.r",
  "}": "delim.brace.r",
  "(": "delim.paren.l",
  "[": "delim.bracket.l",
  "{": "delim.brace.l",
};

function typeTextChar(state: EditorState, ch: string): EditorState {
  const text = textNodeAt(state.root, state.caret.path);
  if (text) {
    const { caret } = state;
    const lo = Math.min(caret.anchor ?? caret.offset, caret.offset);
    const hi = Math.max(caret.anchor ?? caret.offset, caret.offset);
    const value = text.text.slice(0, lo) + ch + text.text.slice(hi);
    const rowPath = caret.path.slice(0, -2);
    const index = caret.path[caret.path.length - 2] as number;
    const root = updateAt(state.root, rowPath, (x) => row(x.children.map((n, i) => (i === index ? { ...text, text: value } : n))));
    return { root, caret: caretAt(caret.path, lo + ch.length) };
  }
  if (/^\s$/.test(ch)) return state;
  if (/^\p{L}$/u.test(ch)) return insertNodes(state, [{ kind: "ident", text: ch }]);
  if (/^[0-9]$/.test(ch)) return insertNodes(state, [{ kind: "number", text: ch }]);
  if (ch === "^") return script(state, "sup");
  if (ch === "_") return script(state, "sub");
  if (ch === "/") return fraction(state);
  if (ch === ".") {
    const r = rowAt(state.root, state.caret.path) as MathRow;
    const prev = r.children[state.caret.offset - 1];
    return insertNodes(state, [prev?.kind === "number" ? { kind: "number", text: "." } : { kind: "symbol", id: "punct.period" }]);
  }
  const open = OPEN_CHARS[ch];
  if (open) {
    const sel = takeSelection(state);
    return insertStructure(sel.state, { kind: "delim", left: open[0], right: open[1], body: row(sel.nodes), form: "plain" }, "body");
  }
  const close = CLOSE_CHARS[ch];
  if (close) {
    // Overtype: at the end of a pair's body closed by this character, step out.
    const { caret } = state;
    const r = rowAt(state.root, caret.path) as MathRow;
    if (caret.path.length >= 2 && caret.offset === r.children.length && caret.anchor === undefined) {
      const parentPath = caret.path.slice(0, -2);
      const index = caret.path[caret.path.length - 2] as number;
      const node = (rowAt(state.root, parentPath) as MathRow).children[index];
      if (node?.kind === "delim" && node.right === close) return { root: state.root, caret: caretAt(parentPath, index + 1) };
    }
  }
  const id = CHAR_SYMBOLS[ch] ?? entryByGlyph(ch)?.id;
  if (id) return insertNodes(state, [{ kind: "symbol", id }]);
  if (/^\p{N}$/u.test(ch)) return insertNodes(state, [{ kind: "number", text: ch }]);
  // Other characters have no math meaning here.
  return state;
}

export function typeText(state: EditorState, text: string): EditorState {
  let s = state;
  for (const ch of text) s = typeTextChar(s, ch);
  return s;
}

const PARTNER = (id: SymbolId): SymbolId | null => {
  if (id.endsWith(".l")) {
    const r = `${id.slice(0, -2)}.r`;
    return entryById(r) ? r : null;
  }
  const pairs: Record<string, string> = { "delim.corner.tl": "delim.corner.tr", "delim.corner.bl": "delim.corner.br" };
  return pairs[id] ?? null;
};

export function insertSymbol(state: EditorState, id: SymbolId): EditorState {
  const e = entryById(id);
  if (!e) return state;
  if (textNodeAt(state.root, state.caret.path)) return typeTextChar(state, e.glyph);
  switch (e.role) {
    case "bigop":
      return insertNodes(state, [{ kind: "bigop", op: id, lower: null, upper: null }]);
    case "func":
      return insertNodes(state, [id === "func.operatorname" ? { kind: "func", name: id, text: "" } : { kind: "func", name: id }]);
    case "accent": {
      const sel = takeSelection(state);
      return insertStructure(sel.state, { kind: "accent", accent: id, body: row(sel.nodes) }, "body");
    }
    case "font": {
      const sel = takeSelection(state);
      return insertStructure(sel.state, { kind: "font", font: id, body: row(sel.nodes) }, "body");
    }
    case "delim": {
      const partner = e.mathClass === "open" ? PARTNER(id) : null;
      if (partner) {
        const sel = takeSelection(state);
        return insertStructure(sel.state, { kind: "delim", left: id, right: partner, body: row(sel.nodes) }, "body");
      }
      return insertNodes(state, [{ kind: "symbol", id }]);
    }
    default:
      return insertNodes(state, [{ kind: "symbol", id }]);
  }
}

function emptyGrid(rows: number, cols: number): MathRow[][] {
  return Array.from({ length: rows }, () => Array.from({ length: cols }, () => row()));
}

export function insertTemplate(state: EditorState, template: TemplateId, options?: { rows?: number; cols?: number }): EditorState {
  if (textNodeAt(state.root, state.caret.path)) return state;
  const sel = takeSelection(state);
  const body = row(sel.nodes);
  const s = sel.state;
  const rows = Math.max(1, options?.rows ?? 2);
  const cols = Math.max(1, options?.cols ?? 2);
  const place = (node: MathNode, after: MathNode[] = []) => insertStructure(s, node, firstEmptySlot(node), after);
  const delim = (l: SymbolId, r: SymbolId): MathNode => ({ kind: "delim", left: l, right: r, body });
  switch (template) {
    case "frac":
      return place({ kind: "frac", num: body, den: row() });
    case "sqrt":
      return place({ kind: "sqrt", body, index: null });
    case "root":
      return place({ kind: "sqrt", body, index: row() });
    case "sup":
      return place({ kind: "scripts", base: body, sub: null, sup: row() });
    case "sub":
      return place({ kind: "scripts", base: body, sub: row(), sup: null });
    case "subsup":
      return place({ kind: "scripts", base: body, sub: row(), sup: row() });
    case "integral":
      return place({ kind: "bigop", op: "bigop.int", lower: row(), upper: row() }, sel.nodes);
    case "sum":
      return place({ kind: "bigop", op: "bigop.sum", lower: row(), upper: row() }, sel.nodes);
    case "product":
      return place({ kind: "bigop", op: "bigop.prod", lower: row(), upper: row() }, sel.nodes);
    case "limit":
      return place({ kind: "bigop", op: "func.lim", lower: row(), upper: null }, sel.nodes);
    case "parens":
      return place(delim("delim.paren.l", "delim.paren.r"));
    case "brackets":
      return place(delim("delim.bracket.l", "delim.bracket.r"));
    case "braces":
      return place(delim("delim.brace.l", "delim.brace.r"));
    case "abs":
      return place(delim("delim.vert", "delim.vert"));
    case "norm":
      return place(delim("delim.Vert", "delim.Vert"));
    case "matrix": {
      const grid = emptyGrid(rows, cols);
      (grid[0] as MathRow[])[0] = body;
      return place({ kind: "matrix", fence: "delim.paren", rows: grid });
    }
    case "cases": {
      const grid = emptyGrid(rows, 2).map((r): [MathRow, MathRow] => [r[0] as MathRow, r[1] as MathRow]);
      (grid[0] as [MathRow, MathRow])[0] = body;
      return place({ kind: "cases", rows: grid });
    }
    case "aligned": {
      const grid = emptyGrid(rows, cols);
      (grid[0] as MathRow[])[0] = body;
      return place({ kind: "aligned", rows: grid });
    }
    case "text":
      return insertStructure(s, { kind: "text", text: "" }, "text", sel.nodes);
  }
}

// ================================================================ deletion

function isEmptyNode(n: MathNode): boolean {
  if (n.kind === "text") return n.text === "";
  if (!hasSlots(n)) return false;
  return slotNames(n).every((s) => isEmptyRow(getSlot(n, s)));
}

/** Nodes deleted with a select-then-delete step when they have content. */
const isCompound = (n: MathNode) => hasSlots(n) || n.kind === "text" || n.kind === "opaque";

function removeRange(state: EditorState, path: SlotPath, lo: number, hi: number, caret: number): EditorState {
  const root = updateAt(state.root, path, (x) => row([...x.children.slice(0, lo), ...x.children.slice(hi)]));
  return { root, caret: caretAt(path, caret) };
}

/** Replace node `index` of row `path` with `nodes`; caret at `path`/`caretOffset`. */
function spliceNode(state: EditorState, path: SlotPath, index: number, nodes: MathNode[], caretOffset: number): EditorState {
  const root = updateAt(state.root, path, (x) => row([...x.children.slice(0, index), ...nodes, ...x.children.slice(index + 1)]));
  return { root, caret: caretAt(path, caretOffset) };
}

function unwrapContents(n: MathNode): MathNode[] | null {
  switch (n.kind) {
    case "delim":
    case "accent":
    case "font":
      return n.body.children;
    case "sqrt":
      return n.index ? null : n.body.children;
    case "frac":
      return [...n.num.children, ...n.den.children];
    default:
      return null;
  }
}

function textDelete(state: EditorState, dir: -1 | 1): EditorState {
  const { caret } = state;
  const text = textNodeAt(state.root, caret.path) as Extract<MathNode, { kind: "text" }>;
  const rowPath = caret.path.slice(0, -2);
  const index = caret.path[caret.path.length - 2] as number;
  const lo = Math.min(caret.anchor ?? caret.offset, caret.offset);
  const hi = Math.max(caret.anchor ?? caret.offset, caret.offset);
  let from = lo;
  let to = hi;
  if (lo === hi) {
    if (dir < 0 && lo === 0) return text.text === "" ? spliceNode(state, rowPath, index, [], index) : { root: state.root, caret: caretAt(rowPath, index) };
    if (dir > 0 && lo === text.text.length) return text.text === "" ? spliceNode(state, rowPath, index, [], index) : { root: state.root, caret: caretAt(rowPath, index + 1) };
    if (dir < 0) from = lo - 1;
    else to = hi + 1;
  }
  const value = text.text.slice(0, from) + text.text.slice(to);
  const root = updateAt(state.root, rowPath, (x) => row(x.children.map((n, i) => (i === index ? { ...text, text: value } : n))));
  return { root, caret: caretAt(caret.path, from) };
}

export function deleteBackward(state: EditorState): EditorState {
  const { root, caret } = state;
  if (isTextPath(caret.path)) return textDelete(state, -1);
  if (caret.anchor !== undefined && caret.anchor !== caret.offset) {
    const lo = Math.min(caret.anchor, caret.offset);
    return removeRange(state, caret.path, lo, Math.max(caret.anchor, caret.offset), lo);
  }
  const r = rowAt(root, caret.path) as MathRow;
  if (caret.offset > 0) {
    const prev = r.children[caret.offset - 1] as MathNode;
    if (isCompound(prev) && !isEmptyNode(prev)) return { root, caret: { path: caret.path, offset: caret.offset, anchor: caret.offset - 1 } };
    return removeRange(state, caret.path, caret.offset - 1, caret.offset, caret.offset - 1);
  }
  if (caret.path.length === 0) return state;
  const parentPath = caret.path.slice(0, -2);
  const index = caret.path[caret.path.length - 2] as number;
  const slot = String(caret.path[caret.path.length - 1]);
  const node = (rowAt(root, parentPath) as MathRow).children[index] as MathNode;
  if (isEmptyNode(node)) return spliceNode(state, parentPath, index, [], index);
  const slots = slotNames(node);
  const pos = slots.indexOf(slot);
  const prevSlot = pos > 0 ? (slots[pos - 1] as string) : null;
  const toPrevSlot = (): EditorState => {
    if (!prevSlot) return { root, caret: caretAt(parentPath, index) };
    return { root, caret: caretAt([...parentPath, index, prevSlot], (getSlot(node, prevSlot) as MathRow).children.length) };
  };
  // Empty optional slots are removed.
  const optional =
    (node.kind === "scripts" && (slot === "sub" || slot === "sup")) ||
    (node.kind === "bigop" && (slot === "lower" || slot === "upper")) ||
    (node.kind === "sqrt" && slot === "index");
  if (optional && isEmptyRow(r)) {
    const updated = setSlot(node, slot, null);
    if (updated.kind === "scripts" && !updated.sub && !updated.sup) {
      return spliceNode(state, parentPath, index, updated.base.children, index + updated.base.children.length);
    }
    const s2 = spliceNode(state, parentPath, index, [updated], index + (node.kind === "bigop" ? 1 : 0));
    if (prevSlot) return { root: s2.root, caret: caretAt([...parentPath, index, prevSlot], (getSlot(updated, prevSlot) as MathRow).children.length) };
    return s2;
  }
  if (pos === 0) {
    const contents = unwrapContents(node);
    if (contents) return spliceNode(state, parentPath, index, contents, index);
  }
  return toPrevSlot();
}

export function deleteForward(state: EditorState): EditorState {
  const { root, caret } = state;
  if (isTextPath(caret.path)) return textDelete(state, 1);
  if (caret.anchor !== undefined && caret.anchor !== caret.offset) {
    const lo = Math.min(caret.anchor, caret.offset);
    return removeRange(state, caret.path, lo, Math.max(caret.anchor, caret.offset), lo);
  }
  const r = rowAt(root, caret.path) as MathRow;
  if (caret.offset < r.children.length) {
    const next = r.children[caret.offset] as MathNode;
    if (isCompound(next) && !isEmptyNode(next)) return { root, caret: { path: caret.path, offset: caret.offset, anchor: caret.offset + 1 } };
    return removeRange(state, caret.path, caret.offset, caret.offset + 1, caret.offset);
  }
  if (caret.path.length === 0) return state;
  const parentPath = caret.path.slice(0, -2);
  const index = caret.path[caret.path.length - 2] as number;
  const slot = String(caret.path[caret.path.length - 1]);
  const node = (rowAt(root, parentPath) as MathRow).children[index] as MathNode;
  if (isEmptyNode(node)) return spliceNode(state, parentPath, index, [], index);
  const slots = slotNames(node);
  const pos = slots.indexOf(slot);
  const nextSlot = pos >= 0 && pos < slots.length - 1 ? (slots[pos + 1] as string) : null;
  if (nextSlot) {
    const nextRow = getSlot(node, nextSlot) as MathRow;
    const optional =
      (node.kind === "scripts" && (nextSlot === "sub" || nextSlot === "sup")) ||
      (node.kind === "bigop" && (nextSlot === "lower" || nextSlot === "upper"));
    if (optional && isEmptyRow(nextRow)) {
      const updated = setSlot(node, nextSlot, null);
      if (updated.kind === "scripts" && !updated.sub && !updated.sup) {
        return spliceNode(state, parentPath, index, updated.base.children, index + updated.base.children.length);
      }
      const root2 = updateAt(root, parentPath, (x) => row(x.children.map((n, i) => (i === index ? updated : n))));
      return { root: root2, caret: caret };
    }
    return { root, caret: caretAt([...parentPath, index, nextSlot], 0) };
  }
  if (slots.length === 1) {
    const contents = unwrapContents(node);
    if (contents) return spliceNode(state, parentPath, index, contents, index + contents.length);
  }
  return { root, caret: caretAt(parentPath, index + 1) };
}

// ================================================================ grids

export function editGrid(state: EditorState, edit: GridEdit): EditorState {
  const { root, caret } = state;
  const path = rowPathOf(caret.path);
  for (let depth = path.length; depth >= 2; depth -= 2) {
    const parentPath = path.slice(0, depth - 2);
    const index = path[depth - 2] as number;
    const cell = parseCell(path[depth - 1]);
    const node = (rowAt(root, parentPath) as MathRow).children[index] as MathNode;
    if (!cell || (node.kind !== "matrix" && node.kind !== "aligned" && node.kind !== "cases")) continue;
    const result = gridEdit(node, cell.r, cell.c, edit);
    if (!result) return state;
    const newRoot = updateAt(root, parentPath, (x) => row(x.children.map((n, i) => (i === index ? result.node : n))));
    const target = getSlot(result.node, cellName(result.r, result.c));
    return { root: newRoot, caret: caretAt([...parentPath, index, cellName(result.r, result.c)], result.end && target ? target.children.length : 0) };
  }
  return state;
}

function gridEdit(node: Extract<MathNode, { kind: "matrix" | "aligned" | "cases" }>, r: number, c: number, edit: GridEdit): { node: MathNode; r: number; c: number; end?: boolean } | null {
  const rows: MathRow[][] = node.rows.map((cells) => [...cells]);
  const width = node.kind === "cases" ? 2 : Math.max(1, ...rows.map((x) => x.length));
  const blank = () => Array.from({ length: width }, () => row());
  const rebuild = (next: MathRow[][]): MathNode =>
    node.kind === "cases" ? { ...node, rows: next.map((x): [MathRow, MathRow] => [x[0] ?? row(), x[1] ?? row()]) } : { ...node, rows: next };
  switch (edit) {
    case "addRowAfter":
      rows.splice(r + 1, 0, blank());
      return { node: rebuild(rows), r: r + 1, c: 0 };
    case "addRowBefore":
      rows.splice(r, 0, blank());
      return { node: rebuild(rows), r, c: 0 };
    case "removeRow":
      if (rows.length <= 1) return null;
      rows.splice(r, 1);
      return { node: rebuild(rows), r: Math.min(r, rows.length - 1), c: Math.min(c, (rows[Math.min(r, rows.length - 1)] as MathRow[]).length - 1), end: true };
    case "addColAfter":
    case "addColBefore": {
      if (node.kind === "cases") return null;
      const at = edit === "addColAfter" ? c + 1 : c;
      for (const cells of rows) cells.splice(Math.min(at, cells.length), 0, row());
      return { node: rebuild(rows), r, c: at };
    }
    case "removeCol": {
      if (node.kind === "cases" || width <= 1) return null;
      for (const cells of rows) if (cells.length > c && cells.length > 1) cells.splice(c, 1);
      return { node: rebuild(rows), r, c: Math.max(0, Math.min(c, (rows[r] as MathRow[]).length - 1)), end: true };
    }
  }
}
