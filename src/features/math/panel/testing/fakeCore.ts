// A small, contract-conformant stand-in for the equation core (lane EQ-A),
// used only by the panel's tests (`vi.mock("../core", ...)`). It implements
// enough of each function for the panel's behavior to be exercised: a tiny
// catalog, a TeX-like/Typst-like/Markdown-like adapter over a small subset,
// and the editing model with the slot-path convention of `../tree.ts`.
// It is not the real core and proves nothing about real language coverage.
import type {
  CatalogEntry,
  EditorState,
  EquationRange,
  MathCaret,
  MathLanguage,
  MathLanguageAdapter,
  MathNode,
  MathRow,
  ParseResult,
  Requirement,
  SerializeResult,
  SlotPath,
  SourceContext,
  TemplateId,
} from "../../core/types";
import { gridSlot, slotsOf } from "../tree";

export type * from "../../core/types";

// ---- catalog

const entry = (
  id: string,
  name: string,
  category: string,
  glyph: string,
  aliases: string[],
  latex: CatalogEntry["latex"],
  typst: string | null,
  markdown = true,
  role: CatalogEntry["role"] = "symbol",
): CatalogEntry => ({ id, name, category, glyph, aliases, role, latex, typst: typst ? { name: typst } : null, markdown });

const ENTRIES: CatalogEntry[] = [
  entry("greek.alpha", "alpha", "greek", "α", ["alpha"], { command: "\\alpha" }, "alpha"),
  entry("greek.beta", "beta", "greek", "β", ["beta"], { command: "\\beta" }, "beta"),
  entry("greek.pi", "pi", "greek", "π", ["pi"], { command: "\\pi" }, "pi"),
  entry("op.plus", "plus", "arithmetic", "+", ["plus", "add"], { command: "+" }, "+"),
  entry("op.minus", "minus", "arithmetic", "−", ["minus", "subtract"], { command: "-" }, "-"),
  entry("rel.eq", "equals", "relations", "=", ["equal", "eq"], { command: "=" }, "="),
  entry("rel.lt", "less than", "relations", "<", ["lt", "less"], { command: "<" }, "<"),
  entry("rel.leq", "less than or equal to", "relations", "≤", ["leq", "le", "<=", "less or equal"], { command: "\\leq" }, "lt.eq"),
  entry("rel.lesssim", "less than or similar to", "relations", "≲", ["lesssim"], { command: "\\lesssim", package: "amssymb" }, "lt.tilde", false),
  entry("set.in", "element of", "sets", "∈", ["in", "member"], { command: "\\in" }, "in"),
  entry("arrow.right", "right arrow", "arrows", "→", ["to", "->", "rightarrow"], { command: "\\rightarrow" }, "arrow.r"),
  entry("logic.forall", "for all", "logic", "∀", ["forall"], { command: "\\forall" }, "forall"),
  entry("misc.typstonly", "typst-only mark", "misc", "⧺", ["typstonly"], null, "plus.double", false),
  entry("op.int", "integral", "large operators", "∫", ["int", "integral"], { command: "\\int" }, "integral", true, "bigop"),
  entry("op.sum", "sum", "large operators", "∑", ["sum", "sigma"], { command: "\\sum" }, "sum", true, "bigop"),
  entry("op.prod", "product", "large operators", "∏", ["prod"], { command: "\\prod" }, "product", true, "bigop"),
  entry("op.lim", "limit", "functions", "lim", ["lim"], { command: "\\lim" }, "lim", true, "bigop"),
  entry("delim.paren.l", "left parenthesis", "delimiters", "(", [], { command: "(" }, "("),
  entry("delim.paren.r", "right parenthesis", "delimiters", ")", [], { command: ")" }, ")"),
  entry("delim.bracket.l", "left bracket", "delimiters", "[", [], { command: "[" }, "["),
  entry("delim.bracket.r", "right bracket", "delimiters", "]", [], { command: "]" }, "]"),
  entry("delim.brace.l", "left brace", "delimiters", "{", [], { command: "\\{" }, "{"),
  entry("delim.brace.r", "right brace", "delimiters", "}", [], { command: "\\}" }, "}"),
  entry("delim.vert", "vertical bar", "delimiters", "|", ["abs"], { command: "|" }, "|"),
  entry("delim.Vert", "double vertical bar", "delimiters", "‖", ["norm"], { command: "\\|" }, "||"),
];

export function languageForPath(path: string): MathLanguage | null {
  const lower = path.toLowerCase();
  if (lower.endsWith(".tex")) return "latex";
  if (lower.endsWith(".typ")) return "typst";
  if (lower.endsWith(".md") || lower.endsWith(".markdown")) return "markdown";
  return null;
}

export const catalog = (): readonly CatalogEntry[] => ENTRIES;
export const catalogEntry = (id: string): CatalogEntry | undefined => ENTRIES.find((e) => e.id === id);

const available = (e: CatalogEntry, language?: MathLanguage) =>
  !language || (language === "latex" ? e.latex !== null : language === "typst" ? e.typst !== null : e.markdown);

export function searchCatalog(query: string, options: { language?: MathLanguage; limit?: number; category?: string }) {
  const q = query.trim().toLowerCase();
  const scored: { entry: CatalogEntry; rank: number; matched: string; order: number }[] = [];
  ENTRIES.forEach((e, order) => {
    if (options.category && e.category !== options.category) return;
    const keys: [string, string][] = [
      [e.name.toLowerCase(), "name"],
      [e.glyph.toLowerCase(), `glyph ${e.glyph}`],
      ...e.aliases.map((a): [string, string] => [a.toLowerCase(), `alias ${a}`]),
      ...(e.latex ? [[e.latex.command.toLowerCase().replace(/^\\/, ""), `command ${e.latex.command}`] as [string, string]] : []),
    ];
    let best: { rank: number; matched: string } | null = null;
    for (const [key, matched] of keys) {
      const rank = key === q ? 0 : key.startsWith(q) ? 1 : key.includes(q) ? 2 : 3;
      if (rank < 3 && (!best || rank < best.rank)) best = { rank, matched };
    }
    if (best) scored.push({ entry: e, ...best, order });
  });
  scored.sort((a, b) => a.rank - b.rank || a.order - b.order);
  return scored
    .slice(0, options.limit ?? 50)
    .map((s) => ({ entry: s.entry, matched: s.matched, available: available(s.entry, options.language) }));
}

// ---- tree utilities

const row = (children: MathNode[] = []): MathRow => ({ kind: "row", children });

function getRow(root: MathRow, path: SlotPath): MathRow {
  let current = root;
  for (let i = 0; i + 1 < path.length; i += 2) {
    const node = current.children[Number(path[i])]!;
    current = slotsOf(node).find((s) => s.name === path[i + 1])!.row;
  }
  return current;
}

function withSlot(node: MathNode, name: string, value: MathRow): MathNode {
  switch (node.kind) {
    case "frac":
      return name === "num" ? { ...node, num: value } : { ...node, den: value };
    case "sqrt":
      return name === "index" ? { ...node, index: value } : { ...node, body: value };
    case "scripts":
      return { ...node, [name]: value };
    case "bigop":
      return { ...node, [name]: value };
    case "delim":
    case "accent":
    case "font":
      return { ...node, body: value };
    case "matrix":
    case "aligned": {
      const [r, c] = name.split(",").map(Number) as [number, number];
      return { ...node, rows: node.rows.map((cells, ri) => (ri === r ? cells.map((cell, ci) => (ci === c ? value : cell)) : cells)) };
    }
    case "cases": {
      const [r, c] = name.split(",").map(Number) as [number, number];
      return {
        ...node,
        rows: node.rows.map((pair, ri) => (ri === r ? ((c === 0 ? [value, pair[1]] : [pair[0], value]) as [MathRow, MathRow]) : pair)),
      };
    }
    default:
      return node;
  }
}

function updateRow(root: MathRow, path: SlotPath, update: (row: MathRow) => MathRow): MathRow {
  if (path.length === 0) return update(root);
  const index = Number(path[0]);
  const node = root.children[index]!;
  const name = String(path[1]);
  const inner = slotsOf(node).find((s) => s.name === name)!.row;
  const replaced = withSlot(node, name, updateRow(inner, path.slice(2), update));
  return row(root.children.map((n, i) => (i === index ? replaced : n)));
}

function insertNodes(state: EditorState, nodes: MathNode[], caretInto?: (index: number) => MathCaret): EditorState {
  const cleared = state.caret.anchor !== undefined ? deleteSelection(state) : state;
  const { path, offset } = cleared.caret;
  const root = updateRow(cleared.root, path, (r) => row([...r.children.slice(0, offset), ...nodes, ...r.children.slice(offset)]));
  return { root, caret: caretInto ? caretInto(offset) : { path, offset: offset + nodes.length } };
}

function deleteSelection(state: EditorState): EditorState {
  const { path, offset, anchor } = state.caret;
  if (anchor === undefined) return state;
  const from = Math.min(anchor, offset);
  const to = Math.max(anchor, offset);
  return { root: updateRow(state.root, path, (r) => row([...r.children.slice(0, from), ...r.children.slice(to)])), caret: { path, offset: from } };
}

function selected(state: EditorState): MathNode[] {
  const { path, offset, anchor } = state.caret;
  if (anchor === undefined) return [];
  const r = getRow(state.root, path);
  return r.children.slice(Math.min(anchor, offset), Math.max(anchor, offset));
}

// ---- editing model

export function createState(root?: MathRow): EditorState {
  const r = root ?? row();
  return { root: r, caret: { path: [], offset: r.children.length } };
}

export type Motion = "left" | "right" | "up" | "down" | "nextSlot" | "prevSlot" | "home" | "end";

function exit(state: EditorState, after: boolean): EditorState {
  const { path } = state.caret;
  if (path.length === 0) return state;
  const parentPath = path.slice(0, -2);
  const index = Number(path[path.length - 2]);
  return { ...state, caret: { path: parentPath, offset: after ? index + 1 : index } };
}

export function move(state: EditorState, motion: Motion, extend?: boolean): EditorState {
  const { path, offset } = state.caret;
  const current = getRow(state.root, path);
  const anchor = extend ? (state.caret.anchor ?? offset) : undefined;
  const at = (o: number): EditorState => ({ ...state, caret: { path, offset: o, ...(anchor !== undefined ? { anchor } : {}) } });
  switch (motion) {
    case "left":
      return offset > 0 ? at(offset - 1) : exit(state, false);
    case "right":
      return offset < current.children.length ? at(offset + 1) : exit(state, true);
    case "home":
      return at(0);
    case "end":
      return at(current.children.length);
    case "nextSlot":
    case "down": {
      if (path.length > 0) {
        const parentPath = path.slice(0, -2);
        const index = Number(path[path.length - 2]);
        const parent = getRow(state.root, parentPath).children[index]!;
        const slots = slotsOf(parent);
        const i = slots.findIndex((s) => s.name === path[path.length - 1]);
        const next = slots[i + 1];
        if (next) return { ...state, caret: { path: [...parentPath, index, next.name], offset: 0 } };
        return exit(state, true);
      }
      const node = current.children[offset];
      const first = node ? slotsOf(node)[0] : undefined;
      return first ? { ...state, caret: { path: [...path, offset, first.name], offset: 0 } } : state;
    }
    case "prevSlot":
    case "up": {
      if (path.length > 0) {
        const parentPath = path.slice(0, -2);
        const index = Number(path[path.length - 2]);
        const parent = getRow(state.root, parentPath).children[index]!;
        const slots = slotsOf(parent);
        const i = slots.findIndex((s) => s.name === path[path.length - 1]);
        const prev = slots[i - 1];
        if (prev) return { ...state, caret: { path: [...parentPath, index, prev.name], offset: prev.row.children.length } };
        return exit(state, false);
      }
      return state;
    }
  }
}

const CHAR_SYMBOLS: Record<string, string> = { "+": "op.plus", "-": "op.minus", "=": "rel.eq", "<": "rel.lt" };

export function typeText(state: EditorState, text: string): EditorState {
  let s = state;
  for (const ch of text) {
    if (ch === " ") continue;
    if (ch === "^") s = insertTemplate(s, "sup");
    else if (ch === "_") s = insertTemplate(s, "sub");
    else if (ch === "/")
      // The item before the caret becomes the numerator.
      s = insertTemplate(s.caret.offset > 0 && s.caret.anchor === undefined ? { ...s, caret: { ...s.caret, anchor: s.caret.offset - 1 } } : s, "frac");
    else if (/\d/.test(ch)) s = insertNodes(s, [{ kind: "number", text: ch }]);
    else if (CHAR_SYMBOLS[ch]) s = insertNodes(s, [{ kind: "symbol", id: CHAR_SYMBOLS[ch]! }]);
    else s = insertNodes(s, [{ kind: "ident", text: ch }]);
  }
  return s;
}

export function insertSymbol(state: EditorState, id: string): EditorState {
  const e = catalogEntry(id);
  if (e?.role === "bigop") return insertNodes(state, [{ kind: "bigop", op: id, lower: row(), upper: row() }]);
  return insertNodes(state, [{ kind: "symbol", id }]);
}

export function insertTemplate(state: EditorState, template: TemplateId, options?: { rows?: number; cols?: number }): EditorState {
  const sel = selected(state);
  const body = row(sel);
  const into = (slot: string) => (index: number): MathCaret => ({ path: [...state.caret.path, index, slot], offset: slot === "num" || slot === "body" || slot === "base" ? body.children.length : 0 });
  const empty = row();
  switch (template) {
    case "frac":
      return insertNodes(state, [{ kind: "frac", num: body, den: empty }], (i) => (sel.length ? { path: [...state.caret.path, i, "den"], offset: 0 } : into("num")(i)));
    case "sqrt":
      return insertNodes(state, [{ kind: "sqrt", body, index: null }], into("body"));
    case "root":
      return insertNodes(state, [{ kind: "sqrt", body, index: row() }], (i) => ({ path: [...state.caret.path, i, "index"], offset: 0 }));
    case "sup":
    case "sub":
    case "subsup": {
      // Typing ^ after an item makes that item the base.
      let s = state;
      let base = body;
      if (!sel.length && s.caret.offset > 0) {
        const r = getRow(s.root, s.caret.path);
        base = row([r.children[s.caret.offset - 1]!]);
        s = { root: updateRow(s.root, s.caret.path, (x) => row(x.children.filter((_, i) => i !== s.caret.offset - 1))), caret: { ...s.caret, offset: s.caret.offset - 1 } };
      }
      const node: MathNode = { kind: "scripts", base, sub: template === "sup" ? null : row(), sup: template === "sub" ? null : row() };
      const target = template === "sub" || template === "subsup" ? "sub" : "sup";
      return insertNodes({ ...s, caret: { path: s.caret.path, offset: s.caret.offset } }, [node], (i) => ({ path: [...s.caret.path, i, base.children.length ? target : "base"], offset: 0 }));
    }
    case "integral":
    case "sum":
    case "product":
    case "limit": {
      const op = { integral: "op.int", sum: "op.sum", product: "op.prod", limit: "op.lim" }[template];
      const upper = template === "limit" ? null : row();
      return insertNodes(state, [{ kind: "bigop", op, lower: row(), upper }], (i) => ({ path: [...state.caret.path, i, "lower"], offset: 0 }));
    }
    case "parens":
    case "brackets":
    case "braces":
    case "abs":
    case "norm": {
      const [left, right] = {
        parens: ["delim.paren.l", "delim.paren.r"],
        brackets: ["delim.bracket.l", "delim.bracket.r"],
        braces: ["delim.brace.l", "delim.brace.r"],
        abs: ["delim.vert", "delim.vert"],
        norm: ["delim.Vert", "delim.Vert"],
      }[template] as [string, string];
      return insertNodes(state, [{ kind: "delim", left, right, body }], into("body"));
    }
    case "matrix": {
      const rows = options?.rows ?? 2;
      const cols = options?.cols ?? 2;
      const grid = Array.from({ length: rows }, () => Array.from({ length: cols }, () => row()));
      return insertNodes(state, [{ kind: "matrix", fence: "delim.paren", rows: grid }], (i) => ({ path: [...state.caret.path, i, gridSlot(0, 0)], offset: 0 }));
    }
    case "cases":
      return insertNodes(state, [{ kind: "cases", rows: [[row(), row()], [row(), row()]] }], (i) => ({ path: [...state.caret.path, i, gridSlot(0, 0)], offset: 0 }));
    case "aligned":
      return insertNodes(state, [{ kind: "aligned", rows: [[row(), row()], [row(), row()]] }], (i) => ({ path: [...state.caret.path, i, gridSlot(0, 0)], offset: 0 }));
    case "text":
      return insertNodes(state, [{ kind: "text", text: "text" }]);
  }
}

export function deleteBackward(state: EditorState): EditorState {
  if (state.caret.anchor !== undefined) return deleteSelection(state);
  const { path, offset } = state.caret;
  if (offset === 0) return exit(state, false);
  return { root: updateRow(state.root, path, (r) => row(r.children.filter((_, i) => i !== offset - 1))), caret: { path, offset: offset - 1 } };
}

export function deleteForward(state: EditorState): EditorState {
  if (state.caret.anchor !== undefined) return deleteSelection(state);
  const { path, offset } = state.caret;
  const r = getRow(state.root, path);
  if (offset >= r.children.length) return state;
  return { root: updateRow(state.root, path, (x) => row(x.children.filter((_, i) => i !== offset))), caret: { path, offset } };
}

export type GridEdit = "addRowAfter" | "addRowBefore" | "addColAfter" | "addColBefore" | "removeRow" | "removeCol";

export function editGrid(state: EditorState, edit: GridEdit): EditorState {
  const { path } = state.caret;
  // The innermost grid on the caret path.
  for (let depth = path.length - 2; depth >= 0; depth -= 2) {
    const parentPath = path.slice(0, depth);
    const index = Number(path[depth]);
    const node = getRow(state.root, parentPath).children[index]!;
    if (node.kind !== "matrix" && node.kind !== "aligned") continue;
    const [r, c] = String(path[depth + 1]).split(",").map(Number) as [number, number];
    const cols = node.rows[0]?.length ?? 1;
    let rows = node.rows;
    if (edit === "addRowAfter" || edit === "addRowBefore") {
      const at = edit === "addRowAfter" ? r + 1 : r;
      rows = [...rows.slice(0, at), Array.from({ length: cols }, () => row()), ...rows.slice(at)];
    } else if (edit === "addColAfter" || edit === "addColBefore") {
      const at = edit === "addColAfter" ? c + 1 : c;
      rows = rows.map((cells) => [...cells.slice(0, at), row(), ...cells.slice(at)]);
    } else if (edit === "removeRow" && rows.length > 1) {
      rows = rows.filter((_, i) => i !== r);
    } else if (edit === "removeCol" && cols > 1) {
      rows = rows.map((cells) => cells.filter((_, i) => i !== c));
    }
    const replaced: MathNode = { ...node, rows };
    const root = updateRow(state.root, parentPath, (x) => row(x.children.map((n, i) => (i === index ? replaced : n))));
    const nr = Math.min(r, rows.length - 1);
    const nc = Math.min(c, (rows[0]?.length ?? 1) - 1);
    return { root, caret: { path: [...parentPath, index, gridSlot(nr, nc)], offset: 0 } };
  }
  return state;
}

export function setCaret(state: EditorState, caret: MathCaret): EditorState {
  return { ...state, caret };
}

export function sameMath(a: MathRow, b: MathRow): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

// ---- serialization

function serializeRow(r: MathRow, language: MathLanguage, out: SerializeResult): string {
  const tokens = r.children.map((n) => serializeNode(n, language, out));
  let text = "";
  for (const t of tokens) {
    if (/\\[a-zA-Z]+$/.test(text) && /^[a-zA-Z0-9]/.test(t)) text += " ";
    else if (language === "typst" && /[a-zA-Z]$/.test(text) && /^[a-zA-Z]/.test(t) && t.length > 1) text += " ";
    text += t;
  }
  return text;
}

function symbolSource(id: string, language: MathLanguage, out: SerializeResult): string {
  const e = catalogEntry(id);
  if (!e) return id;
  if (language === "typst") {
    if (!e.typst) out.unsupported.push({ what: id, reason: "no Typst form" });
    return e.typst?.name ?? "";
  }
  if (!e.latex || (language === "markdown" && !e.markdown)) {
    out.unsupported.push({ what: id, reason: `no ${language === "markdown" ? "Markdown" : "LaTeX"} form` });
    return "";
  }
  if (e.latex.package && language === "latex") out.requirements.push({ kind: "latexPackage", name: e.latex.package });
  return e.latex.command;
}

function serializeNode(n: MathNode, language: MathLanguage, out: SerializeResult): string {
  const s = (r: MathRow) => serializeRow(r, language, out);
  const tex = language !== "typst";
  switch (n.kind) {
    case "ident":
    case "number":
      return n.text;
    case "symbol":
    case "func":
      return symbolSource(n.kind === "symbol" ? n.id : n.name, language, out);
    case "text":
      return tex ? `\\text{${n.text}}` : `"${n.text}"`;
    case "frac":
      return tex ? `\\frac{${s(n.num)}}{${s(n.den)}}` : `(${s(n.num)})/(${s(n.den)})`;
    case "sqrt":
      if (tex) return n.index ? `\\sqrt[${s(n.index)}]{${s(n.body)}}` : `\\sqrt{${s(n.body)}}`;
      return n.index ? `root(${s(n.index)}, ${s(n.body)})` : `sqrt(${s(n.body)})`;
    case "scripts":
      return `${s(n.base)}${n.sub ? `_${tex ? `{${s(n.sub)}}` : `(${s(n.sub)})`}` : ""}${n.sup ? `^${tex ? `{${s(n.sup)}}` : `(${s(n.sup)})`}` : ""}`;
    case "bigop":
      return `${symbolSource(n.op, language, out)}${n.lower ? `_{${s(n.lower)}}` : ""}${n.upper ? `^{${s(n.upper)}}` : ""}`;
    case "delim":
      return tex
        ? `\\left${symbolSource(n.left, language, out)}${s(n.body)}\\right${symbolSource(n.right, language, out)}`
        : `lr(${symbolSource(n.left, language, out)}${s(n.body)}${symbolSource(n.right, language, out)})`;
    case "accent":
    case "font":
      return `${symbolSource(n.kind === "accent" ? n.accent : n.font, language, out)}{${s(n.body)}}`;
    case "matrix":
      return tex
        ? `\\begin{pmatrix}${n.rows.map((cells) => cells.map(s).join(" & ")).join(" \\\\ ")}\\end{pmatrix}`
        : `mat(${n.rows.map((cells) => cells.map(s).join(", ")).join("; ")})`;
    case "cases":
      return tex
        ? `\\begin{cases}${n.rows.map(([v, c]) => `${s(v)} & ${s(c)}`).join(" \\\\ ")}\\end{cases}`
        : `cases(${n.rows.map(([v, c]) => `${s(v)} & ${s(c)}`).join(", ")})`;
    case "aligned":
      return tex
        ? `\\begin{aligned}${n.rows.map((cells) => cells.map(s).join(" &")).join(" \\\\ ")}\\end{aligned}`
        : n.rows.map((cells) => cells.map(s).join(" &")).join(" \\ ");
    case "opaque":
      return n.source;
  }
}

export function previewLatex(r: MathRow): string {
  return serializeRow(r, "latex", { source: "", requirements: [], unsupported: [] });
}

// ---- parsing (a tiny TeX/Typst subset; anything else is opaque)

function parseContent(content: string, language: MathLanguage): ParseResult {
  const nodes: MathNode[] = [];
  let complete = true;
  let i = 0;
  const readGroup = (): string | null => {
    if (content[i] !== "{") return null;
    let depth = 0;
    const start = i;
    for (; i < content.length; i++) {
      if (content[i] === "{") depth++;
      else if (content[i] === "}" && --depth === 0) {
        i++;
        return content.slice(start + 1, i - 1);
      }
    }
    return null;
  };
  while (i < content.length) {
    const ch = content[i]!;
    if (/\s/.test(ch)) {
      i++;
    } else if (/[a-zA-Z]/.test(ch)) {
      nodes.push({ kind: "ident", text: ch });
      i++;
    } else if (/\d/.test(ch)) {
      nodes.push({ kind: "number", text: ch });
      i++;
    } else if (CHAR_SYMBOLS[ch]) {
      nodes.push({ kind: "symbol", id: CHAR_SYMBOLS[ch]! });
      i++;
    } else if (ch === "^" || ch === "_") {
      i++;
      const group = readGroup() ?? content[i++] ?? "";
      const inner = parseContent(group, language);
      complete &&= inner.complete;
      const prev = nodes.pop();
      const base = prev?.kind === "scripts" ? prev.base : row(prev ? [prev] : []);
      const keep = prev?.kind === "scripts" ? prev : null;
      nodes.push({
        kind: "scripts",
        base,
        sub: ch === "_" ? inner.row : (keep?.sub ?? null),
        sup: ch === "^" ? inner.row : (keep?.sup ?? null),
      });
    } else if (ch === "\\" && language !== "typst") {
      const name = /^\\[a-zA-Z]+/.exec(content.slice(i))?.[0] ?? content.slice(i, i + 2);
      const start = i;
      i += name.length;
      if (name === "\\frac") {
        const num = readGroup();
        const den = readGroup();
        if (num !== null && den !== null) {
          const a = parseContent(num, language);
          const b = parseContent(den, language);
          complete &&= a.complete && b.complete;
          nodes.push({ kind: "frac", num: a.row, den: b.row });
          continue;
        }
      }
      const known = ENTRIES.find((e) => e.latex?.command === name && e.role === "symbol");
      if (known) {
        nodes.push({ kind: "symbol", id: known.id });
        continue;
      }
      // Unknown macro: keep it (with a following group) exactly as written.
      if (content[i] === "{") readGroup();
      nodes.push({ kind: "opaque", source: content.slice(start, i), language, reason: `Unknown command ${name}` });
      complete = false;
    } else {
      nodes.push({ kind: "opaque", source: ch, language, reason: `Unsupported character ${ch}` });
      complete = false;
      i++;
    }
  }
  return { row: row(nodes), complete };
}

// ---- equation location

function findEquations(text: string, language: MathLanguage): EquationRange[] {
  const found: EquationRange[] = [];
  const patterns: [RegExp, boolean, string][] =
    language === "latex"
      ? [
          [/\\begin\{equation\}([\s\S]*?)\\end\{equation\}/g, true, "equation"],
          [/\\\[([\s\S]*?)\\\]/g, true, "\\["],
          [/(?<!\\)\$([^$]+?)\$/g, false, "$"],
        ]
      : language === "markdown"
        ? [
            [/\$\$([\s\S]+?)\$\$/g, true, "$$"],
            [/(?<!\$)\$([^$\n]+?)\$(?!\$)/g, false, "$"],
          ]
        : [[/\$([^$]*?)\$/g, false, "typst"]];
  for (const [re, display, style] of patterns) {
    for (const m of text.matchAll(re)) {
      const start = m.index!;
      const end = start + m[0].length;
      if (found.some((e) => start < e.end && end > e.start)) continue;
      const contentStart = start + m[0].indexOf(m[1]!, style === "equation" ? 16 : 1);
      const content = m[1]!;
      const typstDisplay = style === "typst" && /^\s/.test(content) && /\s$/.test(content) && content.trim().length > 0;
      found.push({
        start,
        end,
        contentStart,
        contentEnd: contentStart + content.length,
        display: style === "typst" ? typstDisplay : display,
        style: style === "typst" ? (typstDisplay ? "typst-block" : "typst-inline") : style,
      });
    }
  }
  return found.sort((a, b) => a.start - b.start);
}

function adapter(language: MathLanguage): MathLanguageAdapter {
  return {
    language,
    equations: (text) => findEquations(text, language),
    contextAt(text, offset): SourceContext {
      const equation = findEquations(text, language).find((e) => e.contentStart <= offset && offset <= e.contentEnd);
      if (equation) return { kind: "math", equation };
      const lineStart = text.lastIndexOf("\n", offset - 1) + 1;
      const line = text.slice(lineStart, text.indexOf("\n", offset) === -1 ? undefined : text.indexOf("\n", offset));
      if (language === "latex" && /^\s*%/.test(line)) return { kind: "unavailable", reason: "inside a comment" };
      if (language === "latex" && text.indexOf("\\begin{document}") > offset) return { kind: "unavailable", reason: "in the preamble" };
      return { kind: "text" };
    },
    parse: (content) => parseContent(content, language),
    serialize(r): SerializeResult {
      const out: SerializeResult = { source: "", requirements: [], unsupported: [] };
      out.source = serializeRow(r, language, out);
      return out;
    },
    wrap(content, display) {
      if (language === "latex") return display ? `\\[\n${content}\n\\]` : `$${content}$`;
      if (language === "markdown") return display ? `$$\n${content}\n$$` : `$${content}$`;
      return display ? `$ ${content} $` : `$${content}$`;
    },
    satisfied: (text, requirement: Requirement) => language !== "latex" || text.includes(`\\usepackage{${requirement.name}}`),
    addRequirement(text, requirement) {
      if (language !== "latex") return null;
      const at = text.indexOf("\\begin{document}");
      return at < 0 ? null : { offset: at, insert: `\\usepackage{${requirement.name}}\n` };
    },
  };
}

export function adapterFor(language: MathLanguage): MathLanguageAdapter {
  return adapter(language);
}
