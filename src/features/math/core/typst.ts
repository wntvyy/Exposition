// Typst math: equation locator (markup / code / math modes), parser, serializer.
import { allEntries, classOf, entryById, entryByGlyph, entryByLatex, entryByTypst } from "./catalog";
import { contextFrom, isLimitFunc, type Region } from "./latex";
import { isComplete, isEmptyRow, row } from "./tree";
import type { CatalogEntry, EquationRange, MathLanguageAdapter, MathNode, MathRow, ParseResult, SerializeResult } from "./types";

// ================================================================ locator

interface TypstScan {
  equations: EquationRange[];
  /** Code, comments, raw text, strings (unavailable) and content blocks inside code (markup, available). */
  regions: (Region & { markup?: boolean })[];
}

const KEYWORDS = new Set(["let", "set", "show", "import", "include", "if", "for", "while", "return", "context", "break", "continue"]);
const isIdStart = (c: string | undefined) => c !== undefined && /[\p{L}_]/u.test(c);
const isIdContinue = (c: string | undefined) => c !== undefined && /[\p{L}\p{N}_-]/u.test(c);

class TypstScanner {
  readonly equations: EquationRange[] = [];
  readonly regions: (Region & { markup?: boolean })[] = [];
  /** Nesting of equations being scanned. */
  private mathDepth = 0;
  /** Set when a comment, string, raw text, code group, or content block runs to the end unclosed. */
  unterminated = false;
  constructor(private readonly text: string) {}

  private lineComment(i: number): number {
    const nl = this.text.indexOf("\n", i);
    const end = nl < 0 ? this.text.length : nl;
    this.regions.push({ start: i, end, reason: "comment", endIn: true });
    return end;
  }

  private blockComment(i: number): number {
    let depth = 0;
    let j = i;
    while (j < this.text.length) {
      if (this.text.startsWith("/*", j)) {
        depth++;
        j += 2;
      } else if (this.text.startsWith("*/", j)) {
        depth--;
        j += 2;
        if (depth === 0) break;
      } else j++;
    }
    if (depth !== 0) this.unterminated = true;
    this.regions.push({ start: i, end: j, reason: "comment" });
    return j;
  }

  private raw(i: number): number {
    let n = 0;
    while (this.text[i + n] === "`") n++;
    if (n === 2) {
      this.regions.push({ start: i, end: i + 2, reason: "raw text" });
      return i + 2;
    }
    const fence = "`".repeat(n);
    const close = this.text.indexOf(fence, i + n);
    const end = close < 0 ? this.text.length : close + n;
    if (close < 0) this.unterminated = true;
    this.regions.push({ start: i, end, reason: "raw text" });
    return end;
  }

  private string(i: number): number {
    let j = i + 1;
    while (j < this.text.length && this.text[j] !== '"') j += this.text[j] === "\\" ? 2 : 1;
    const end = Math.min(this.text.length, j + 1);
    if (j >= this.text.length) this.unterminated = true;
    this.regions.push({ start: i, end, reason: "string" });
    return end;
  }

  /** Markup until `]` closing a content block (when `inBlock`) or the end. Returns the offset of `]` or the end. */
  markup(i: number, inBlock: boolean): number {
    let depth = 0;
    let j = i;
    const t = this.text;
    while (j < t.length) {
      const c = t[j];
      if (c === "\\") {
        j += 2;
        continue;
      }
      if (c === "/" && t[j + 1] === "/" && !/https?:$/.test(t.slice(Math.max(0, j - 6), j))) {
        j = this.lineComment(j);
        continue;
      }
      if (c === "/" && t[j + 1] === "*") {
        j = this.blockComment(j);
        continue;
      }
      if (c === "`") {
        j = this.raw(j);
        continue;
      }
      if (c === "$") {
        j = this.math(j);
        continue;
      }
      if (c === "#" && (isIdStart(t[j + 1]) || "{[(\"".includes(t[j + 1] ?? " "))) {
        j = this.embedded(j);
        continue;
      }
      if (c === "[") depth++;
      if (c === "]") {
        if (depth === 0 && inBlock) return j;
        depth = Math.max(0, depth - 1);
      }
      j++;
    }
    return j;
  }

  /** An equation starting at the `$` at `i`; returns the offset after its closing `$`. */
  math(i: number): number {
    this.mathDepth++;
    try {
      return this.mathBody(i);
    } finally {
      this.mathDepth--;
    }
  }

  private mathBody(i: number): number {
    const t = this.text;
    let j = i + 1;
    while (j < t.length) {
      const c = t[j];
      if (c === "\\") {
        j += 2;
        continue;
      }
      if (c === '"') {
        j = this.string(j);
        continue;
      }
      if (c === "/" && t[j + 1] === "/") {
        j = this.lineComment(j);
        continue;
      }
      if (c === "/" && t[j + 1] === "*") {
        j = this.blockComment(j);
        continue;
      }
      if (c === "#") {
        j = this.embedded(j);
        continue;
      }
      if (c === "$") break;
      j++;
    }
    if (j >= t.length) {
      // Unterminated: not an equation; treat as text up to the end.
      return t.length;
    }
    const content = t.slice(i + 1, j);
    const block = content.length > 0 && /^\s/.test(content) && /\s$/.test(content);
    // An equation nested in an equation (`$x #[$y$] z$`) is part of the outer one.
    if (this.mathDepth === 1) this.equations.push({ start: i, end: j + 1, contentStart: i + 1, contentEnd: j, display: block, style: block ? "typst-block" : "typst-inline" });
    return j + 1;
  }

  /** An embedded code expression after `#` at `i`; returns its end. */
  embedded(i: number): number {
    const t = this.text;
    let j = i + 1;
    const start = i;
    const finish = (end: number) => {
      this.regions.push({ start, end, reason: "code" });
      return end;
    };
    const c = t[j];
    if (c === "{") return finish(this.code(j + 1, "}") + 1);
    if (c === "(") return finish(this.code(j + 1, ")") + 1);
    if (c === "[") return finish(this.contentBlock(j));
    if (c === '"') return finish(this.string(j));
    if (!isIdStart(c)) return i + 1;
    while (isIdContinue(t[j])) j++;
    const word = t.slice(i + 1, j);
    if (KEYWORDS.has(word)) return finish(this.codeLine(j));
    // Postfix chain: fields, calls, trailing content blocks.
    for (;;) {
      if (t[j] === "." && isIdStart(t[j + 1])) {
        j++;
        while (isIdContinue(t[j])) j++;
        continue;
      }
      if (t[j] === "(") {
        j = this.code(j + 1, ")") + 1;
        continue;
      }
      if (t[j] === "[") {
        j = this.contentBlock(j);
        continue;
      }
      break;
    }
    return finish(j);
  }

  private contentBlock(i: number): number {
    const close = this.markup(i + 1, true);
    if (close >= this.text.length) this.unterminated = true;
    this.regions.push({ start: i + 1, end: close, reason: "content", markup: true, startIn: true, endIn: true });
    return Math.min(this.text.length, close + 1);
  }

  /** Code until the matching `close`; returns its offset (or the end). */
  code(i: number, close: string): number {
    const end = this.codeUntil(i, close);
    if (end >= this.text.length) this.unterminated = true;
    return end;
  }

  private codeUntil(i: number, close: string): number {
    const t = this.text;
    let j = i;
    while (j < t.length) {
      const c = t[j];
      if (c === close) return j;
      if (c === '"') j = this.string(j);
      else if (c === "/" && t[j + 1] === "/") j = this.lineComment(j);
      else if (c === "/" && t[j + 1] === "*") j = this.blockComment(j);
      else if (c === "(") j = this.code(j + 1, ")") + 1;
      else if (c === "{") j = this.code(j + 1, "}") + 1;
      else if (c === "[") j = this.contentBlock(j);
      else if (c === "$") j = this.math(j);
      else if (c === "`") j = this.raw(j);
      else j++;
    }
    return j;
  }

  /** A statement (`let`, `set`, `show`, ...) up to the end of its line. */
  private codeLine(i: number): number {
    const t = this.text;
    let j = i;
    while (j < t.length && t[j] !== "\n") {
      const c = t[j];
      if (c === '"') j = this.string(j);
      else if (c === "/" && t[j + 1] === "/") return this.lineComment(j);
      else if (c === "/" && t[j + 1] === "*") j = this.blockComment(j);
      else if (c === "(") j = this.code(j + 1, ")") + 1;
      else if (c === "{") j = this.code(j + 1, "}") + 1;
      else if (c === "[") j = this.contentBlock(j);
      else if (c === "$") j = this.math(j);
      else if (c === ";") return j + 1;
      else j++;
    }
    return j;
  }
}

export function scanTypst(text: string): TypstScan {
  const s = new TypstScanner(text);
  s.markup(0, false);
  s.equations.sort((a, b) => a.start - b.start);
  return { equations: s.equations, regions: s.regions };
}

function typstContext(text: string, offset: number) {
  const scan = scanTypst(text);
  const eq = contextFrom(scan.equations, [], offset);
  if (eq.kind !== "text") return eq;
  // Innermost region wins (a content block inside code is markup again).
  let best: (Region & { markup?: boolean }) | null = null;
  for (const r of scan.regions) {
    const afterStart = offset > r.start || (r.startIn === true && offset === r.start);
    const beforeEnd = offset < r.end || (r.endIn === true && offset === r.end);
    if (afterStart && beforeEnd && (!best || r.end - r.start <= best.end - best.start)) best = r;
  }
  if (!best || best.markup) return { kind: "text" as const };
  return { kind: "unavailable" as const, reason: best.reason };
}

// ================================================================ parser

/** Typst math shorthands (typst-syntax `MathShorthand::LIST`), longest first. */
const SHORTHANDS: [string, string][] = (
  [
    ["...", "…"],
    ["-", "−"],
    ["*", "∗"],
    ["~", "∼"],
    ["!=", "≠"],
    [":=", "≔"],
    ["::=", "⩴"],
    ["=:", "≕"],
    ["<<", "≪"],
    ["<<<", "⋘"],
    [">>", "≫"],
    [">>>", "⋙"],
    ["<=", "≤"],
    [">=", "≥"],
    ["->", "→"],
    ["-->", "⟶"],
    ["|->", "↦"],
    [">->", "↣"],
    ["->>", "↠"],
    ["<-", "←"],
    ["<--", "⟵"],
    ["<-<", "↢"],
    ["<<-", "↞"],
    ["<->", "↔"],
    ["<-->", "⟷"],
    ["~>", "⇝"],
    ["~~>", "⟿"],
    ["<~", "⇜"],
    ["<~~", "⬳"],
    ["=>", "⇒"],
    ["|=>", "⤇"],
    ["==>", "⟹"],
    ["<==", "⟸"],
    ["<=>", "⇔"],
    ["<==>", "⟺"],
    ["[|", "⟦"],
    ["|]", "⟧"],
    ["||", "‖"],
  ] as [string, string][]
).sort((a, b) => b[0].length - a[0].length);

/** Preferred shorthand per glyph for serialization (readable, unambiguous ones only). */
const SHORTHAND_OUT = new Map<string, string>([
  ["≠", "!="],
  ["≤", "<="],
  ["≥", ">="],
  ["→", "->"],
  ["←", "<-"],
  ["↔", "<->"],
  ["⇒", "=>"],
  ["⇔", "<=>"],
  ["↦", "|->"],
  ["⟶", "-->"],
  ["⟹", "==>"],
  ["≪", "<<"],
  ["≫", ">>"],
  ["…", "..."],
  ["−", "-"],
  ["‖", "||"],
  ["≔", ":="],
]);

/** Typst names that are not the catalog's canonical name. */
const TYPST_ALIASES: Record<string, string> = {
  oo: "calc.infty",
  gradient: "calc.nabla",
  nabla: "calc.nabla",
  // A symbol name without modifiers is its first variant (codex): `dot` is `dot.op`.
  dot: "op.cdot",
  dots: "dots.c",
  arrow: "arrow.r",
  arrows: "arrows.rr",
  harpoon: "harpoon.rt",
  harpoons: "harpoons.rtlb",
  tack: "rel.vdash",
  bar: "delim.vert",
  ast: "op.ast",
  tilde: "rel.sim",
  star: "op.star",
  paren: "delim.paren.l",
  brace: "delim.brace.l",
  bracket: "delim.bracket.l",
  chevron: "delim.angle.l",
  ceil: "delim.ceil.l",
  floor: "delim.floor.l",
  corner: "delim.corner.tl",
  mustache: "delim.moustache.l",
  circle: "misc.circ.big",
  square: "misc.square",
  lozenge: "misc.lozenge",
  without: "op.setminus",
  "angle.l": "delim.angle.l",
  "angle.r": "delim.angle.r",
  "dots.c": "dots.c",
  "arrow.r.bar": "arrow.mapsto",
  "planck.reduce": "letter.hbar",
  "integral.cont": "bigop.oint",
};

const OPENINGS = new Map<string, string>([
  ["(", "delim.paren.l"],
  ["[", "delim.bracket.l"],
  ["{", "delim.brace.l"],
  ["⟨", "delim.angle.l"],
  ["⌊", "delim.floor.l"],
  ["⌈", "delim.ceil.l"],
  ["⟦", "delim.dbracket.l"],
]);
const CLOSINGS = new Map<string, string>([
  [")", "delim.paren.r"],
  ["]", "delim.bracket.r"],
  ["}", "delim.brace.r"],
  ["⟩", "delim.angle.r"],
  ["⌋", "delim.floor.r"],
  ["⌉", "delim.ceil.r"],
  ["⟧", "delim.dbracket.r"],
]);

const FONT_FUNCS = new Set(["bb", "cal", "scr", "frak", "bold", "upright", "sans", "italic", "mono"]);

type TK = "space" | "comment" | "str" | "code" | "linebreak" | "esc" | "short" | "amp" | "sub" | "sup" | "slash" | "primes" | "open" | "close" | "ident" | "letter" | "number" | "char";
interface Tk {
  k: TK;
  v: string;
  s: number;
  e: number;
  /** Glyph for shorthands, escapes, openings, closings. */
  g?: string;
}

function lexTypst(src: string): { toks: Tk[]; unterminated: boolean } {
  const out: Tk[] = [];
  let i = 0;
  const scanner = new TypstScanner(src);
  while (i < src.length) {
    const c = src[i] as string;
    const s = i;
    const push = (k: TK, e: number, g?: string) => {
      out.push(g === undefined ? { k, v: src.slice(s, e), s, e } : { k, v: src.slice(s, e), s, e, g });
      i = e;
    };
    if (/\s/.test(c)) {
      let j = i;
      while (j < src.length && /\s/.test(src[j] as string)) j++;
      push("space", j);
      continue;
    }
    if (src.startsWith("//", i)) {
      const nl = src.indexOf("\n", i);
      push("comment", nl < 0 ? src.length : nl);
      continue;
    }
    if (src.startsWith("/*", i)) {
      let depth = 0;
      let j = i;
      while (j < src.length) {
        if (src.startsWith("/*", j)) {
          depth++;
          j += 2;
        } else if (src.startsWith("*/", j)) {
          depth--;
          j += 2;
          if (depth === 0) break;
        } else j++;
      }
      if (depth !== 0) scanner.unterminated = true;
      push("comment", j);
      continue;
    }
    if (c === '"') {
      let j = i + 1;
      while (j < src.length && src[j] !== '"') j += src[j] === "\\" ? 2 : 1;
      if (j >= src.length) scanner.unterminated = true;
      push("str", Math.min(src.length, j + 1));
      continue;
    }
    if (c === "#") {
      push("code", Math.max(i + 1, scanner.embedded(i)));
      continue;
    }
    if (c === "\\") {
      const n = src[i + 1];
      if (n === undefined || /\s/.test(n)) {
        push("linebreak", i + 1);
        continue;
      }
      if (n === "u" && src[i + 2] === "{") {
        const close = src.indexOf("}", i + 3);
        if (close > 0) {
          const cp = parseInt(src.slice(i + 3, close), 16);
          push("esc", close + 1, Number.isFinite(cp) ? String.fromCodePoint(cp) : "");
          continue;
        }
      }
      const cp = src.codePointAt(i + 1) as number;
      push("esc", i + 1 + (cp > 0xffff ? 2 : 1), String.fromCodePoint(cp));
      continue;
    }
    const sh = SHORTHANDS.find(([k]) => src.startsWith(k, i));
    if (sh) {
      const g = sh[1];
      if (OPENINGS.has(g)) push("open", i + sh[0].length, g);
      else if (CLOSINGS.has(g)) push("close", i + sh[0].length, g);
      else push("short", i + sh[0].length, g);
      continue;
    }
    if (c === "&") {
      push("amp", i + 1);
      continue;
    }
    if (c === "_") {
      push("sub", i + 1);
      continue;
    }
    if (c === "^") {
      push("sup", i + 1);
      continue;
    }
    if (c === "/") {
      push("slash", i + 1);
      continue;
    }
    if (c === "'") {
      let j = i;
      while (src[j] === "'") j++;
      push("primes", j);
      continue;
    }
    if (OPENINGS.has(c)) {
      push("open", i + 1, c);
      continue;
    }
    if (CLOSINGS.has(c)) {
      push("close", i + 1, c);
      continue;
    }
    if (/[0-9]/.test(c)) {
      let j = i;
      while (/[0-9]/.test(src[j] ?? "")) j++;
      if (src[j] === "." && /[0-9]/.test(src[j + 1] ?? "")) {
        j++;
        while (/[0-9]/.test(src[j] ?? "")) j++;
      }
      push("number", j);
      continue;
    }
    if (/\p{L}/u.test(c)) {
      let j = i + 1;
      while (j < src.length && /[\p{L}\p{N}\p{M}]/u.test(src[j] as string)) j++;
      if (j === i + 1) {
        push("letter", j);
        continue;
      }
      // Field access: `arrow.r.long`.
      while (src[j] === "." && /\p{L}/u.test(src[j + 1] ?? "")) {
        j++;
        while (j < src.length && /[\p{L}\p{N}\p{M}]/u.test(src[j] as string)) j++;
      }
      push("ident", j);
      continue;
    }
    const cp = src.codePointAt(i) as number;
    push("char", i + (cp > 0xffff ? 2 : 1));
  }
  return { toks: out, unterminated: scanner.unterminated };
}

/** Split call arguments at top-level `,` (and `;` rows) respecting nesting and strings. */
function splitArgs(src: string): { rows: string[][]; offsets: number[][] } {
  const rows: string[][] = [[]];
  const offsets: number[][] = [[]];
  let depth = 0;
  let start = 0;
  let i = 0;
  const pushArg = (end: number) => {
    (rows[rows.length - 1] as string[]).push(src.slice(start, end));
    (offsets[offsets.length - 1] as number[]).push(start);
  };
  while (i < src.length) {
    const c = src[i] as string;
    if (c === "\\") {
      i += 2;
      continue;
    }
    if (c === '"') {
      i++;
      while (i < src.length && src[i] !== '"') i += src[i] === "\\" ? 2 : 1;
      i++;
      continue;
    }
    if ("([{".includes(c)) depth++;
    else if (")]}".includes(c)) depth--;
    else if (depth === 0 && (c === "," || c === ";")) {
      pushArg(i);
      start = i + 1;
      if (c === ";") {
        rows.push([]);
        offsets.push([]);
      }
    }
    i++;
  }
  pushArg(src.length);
  return { rows, offsets };
}

/** A juxtaposition unit (`f(x)`, `n!`, `f'`): one operand for `/` and scripts, flattened in rows. */
interface Juxt {
  kind: "juxt";
  nodes: MathNode[];
}
type PNode = MathNode | Juxt;

function flat(n: PNode): MathNode[] {
  if (n.kind === "juxt") return n.nodes;
  if (n.kind === "number" && n.text.length > 1) return [...n.text].map((d): MathNode => ({ kind: "number", text: d }));
  return [n];
}

const juxt = (base: PNode, more: MathNode[]): Juxt => ({ kind: "juxt", nodes: [...flat(base), ...more] });

class TypstParser {
  private pos = 0;
  private readonly toks: Tk[];
  private readonly unterminated: boolean;
  /** Delim nodes that came from bare parentheses (stripped as fraction/script operands). */
  private readonly bare = new WeakSet<MathNode>();
  constructor(private readonly src: string) {
    const lexed = lexTypst(src);
    this.toks = lexed.toks;
    this.unterminated = lexed.unterminated;
  }

  /** The whole content as one opaque node, without surrounding white space (the layout keeps it). */
  private wholeOpaque(reason: string): MathNode {
    const start = this.src.length - this.src.trimStart().length;
    return this.opaque(start, this.src.trimEnd().length, reason);
  }

  private opaque(s: number, e: number, reason: string): MathNode {
    return { kind: "opaque", source: this.src.slice(s, e), language: "typst", reason };
  }

  private peek(): Tk | undefined {
    return this.toks[this.pos];
  }

  private delimitersBalanced(): boolean {
    let depth = 0;
    for (const t of this.toks) {
      if (t.k === "open") depth++;
      else if (t.k === "close" && --depth < 0) return false;
    }
    return depth === 0;
  }

  /** A dangling `_`/`^` keeps the rest of its row verbatim (nothing may become its operand later). */
  private opaqueRest(start: number, stop: (t: Tk) => boolean, closeStops: boolean, reason: string): MathNode {
    let depth = 0;
    while (this.pos < this.toks.length) {
      const t = this.toks[this.pos] as Tk;
      if (depth === 0 && (stop(t) || (t.k === "close" && closeStops))) break;
      if (t.k === "open") depth++;
      if (t.k === "close") depth--;
      this.pos++;
    }
    let end = start + 1;
    for (let p = this.pos - 1; p >= 0; p--) {
      const t = this.toks[p] as Tk;
      if (t.k !== "space") {
        end = Math.max(end, t.e);
        break;
      }
    }
    return this.opaque(start, end, reason);
  }

  private skipSpace(): void {
    while (this.toks[this.pos]?.k === "space") this.pos++;
  }

  parseContent(): MathRow {
    // Unclosed strings, comments, or code cannot be structured safely: the content stays verbatim.
    if (this.unterminated) return row([this.wholeOpaque("unterminated code, string, or comment")]);
    // Unmatched delimiters make Typst's grouping ambiguous to reproduce: keep the content verbatim.
    if (!this.delimitersBalanced()) return row(this.src.trim() ? [this.wholeOpaque("unmatched delimiters")] : []);
    const rows = this.parseCells();
    if (rows.length === 1 && rows[0]?.length === 1) return rows[0][0] as MathRow;
    return row([{ kind: "aligned", rows, bare: true }]);
  }

  private parseCells(): MathRow[][] {
    const rows: MathRow[][] = [[]];
    let rowStart = -1;
    for (;;) {
      const cell = this.parseRow((t) => t.k === "amp" || t.k === "linebreak");
      (rows[rows.length - 1] as MathRow[]).push(cell);
      const t = this.peek();
      if (!t) break;
      this.pos++;
      if (t.k === "linebreak") {
        rows.push([]);
        rowStart = this.pos;
      }
    }
    // A final line break followed by nothing ends the last row.
    const last = rows[rows.length - 1];
    const blank = rowStart >= 0 && this.toks.slice(rowStart).every((t) => t.k === "space");
    return blank && rows.length > 1 && last?.length === 1 && isEmptyRow(last[0]) ? rows.slice(0, -1) : rows;
  }

  /** Parse until `stop` (not consumed), an unmatched closing delimiter, or the end. */
  private parseRow(stop: (t: Tk) => boolean, closeStops = false): MathRow {
    const units: PNode[] = [];
    for (;;) {
      this.skipSpace();
      const t = this.peek();
      if (!t || stop(t)) break;
      if (t.k === "close" && closeStops) break;
      if (t.k === "slash") {
        this.pos++;
        // The left operand is the previous unit (primary, implicit call, attachments).
        const prev = units.pop();
        const num = prev ? this.operand(prev) : row();
        this.skipSpace();
        const nextTok = this.peek();
        if (!nextTok || stop(nextTok) || (nextTok.k === "close" && closeStops)) {
          if (prev) units.push(prev);
          units.push(this.opaque(t.s, t.e, "fraction without denominator"));
          continue;
        }
        if (["slash", "amp", "linebreak", "sub", "sup"].includes(nextTok.k)) {
          if (prev) units.push(prev);
          units.push(this.opaqueRest(t.s, stop, closeStops, "fraction without denominator"));
          continue;
        }
        const den = this.unit(stop, closeStops);
        units.push({ kind: "frac", num, den: den ? this.operand(den) : row(), variant: "slash" });
        continue;
      }
      const u = this.unit(stop, closeStops);
      if (u) units.push(u);
    }
    return row(units.flatMap(flat));
  }

  /** Strip the parentheses of a bare group used as an operand. */
  private operand(n: PNode): MathRow {
    if (n.kind === "juxt") return row(n.nodes);
    if (n.kind === "delim" && this.bare.has(n) && n.left === "delim.paren.l" && n.right === "delim.paren.r") return n.body;
    return row(flat(n));
  }

  /** A primary (with an implicit call) and its postfix operators and attachments. */
  private unit(stop: (t: Tk) => boolean, closeStops: boolean): PNode | null {
    let base = this.callable(stop, closeStops);
    for (;;) {
      const t = this.peek();
      const prevTok = this.toks[this.pos - 1];
      if (!t || !base) break;
      const adjacent = prevTok !== undefined && prevTok.e === t.s;
      if (t.k === "primes" && adjacent) {
        this.pos++;
        base = juxt(base, Array.from({ length: t.v.length }, (): MathNode => ({ kind: "symbol", id: "calc.prime" })));
        continue;
      }
      if (t.k === "char" && t.v === "!" && adjacent) {
        this.pos++;
        base = juxt(base, [{ kind: "symbol", id: "punct.excl" }]);
        continue;
      }
      // `_` and `^` attach across white space (primes and `!` only when adjacent).
      const op = t.k === "space" ? this.peekNonSpace() : t;
      if (!op || (op.k !== "sub" && op.k !== "sup")) break;
      this.skipSpace();
      this.pos++;
      const argNode = this.callable(stop, closeStops, true);
      if (!argNode) {
        // `x_` without an operand is a syntax error in Typst: keep the operator as source.
        base = juxt(base, [this.opaqueRest(op.s, stop, closeStops, "missing attachment")]);
        break;
      }
      // Postfix `!` and primes directly after the operand belong to it (`x^n!` is x^(n!)).
      let argUnit: PNode = argNode;
      for (;;) {
        const n = this.peek();
        const before = this.toks[this.pos - 1];
        if (!n || !before || before.e !== n.s) break;
        if (n.k === "char" && n.v === "!") argUnit = juxt(argUnit, [{ kind: "symbol", id: "punct.excl" }]);
        else if (n.k === "primes") argUnit = juxt(argUnit, Array.from({ length: n.v.length }, (): MathNode => ({ kind: "symbol", id: "calc.prime" })));
        else break;
        this.pos++;
      }
      const arg = this.operand(argUnit);
      const which = op.k;
      const big = which === "sup" ? "upper" : "lower";
      if (base.kind === "bigop" && base[big] === null) base = { ...base, [big]: arg };
      else if (base.kind === "func" && isLimitFunc(base.name) && base.text === undefined)
        base = { kind: "bigop", op: base.name, lower: null, upper: null, [big]: arg };
      else if (base.kind === "scripts" && base[which] === null) base = { ...base, [which]: arg };
      else base = { kind: "scripts", base: row(flat(base)), sub: null, sup: null, [which]: arg };
    }
    return base;
  }

  /** A primary; a continuable one (letter, name, string, escape) directly followed by a delimiter groups with it. */
  private callable(stop: (t: Tk) => boolean, closeStops: boolean, arg = false): PNode | null {
    const first = this.peekNonSpace();
    if (first?.k === "str" && first.v === '""' && !stop(first)) {
      // An empty string is an empty operand (the writer uses it for empty bases and cells).
      this.skipSpace();
      this.pos++;
      return { kind: "juxt", nodes: [] };
    }
    const p = this.primary(stop, closeStops, arg);
    if (!p || !first) return p;
    const last = this.toks[this.pos - 1];
    // A name that consumed its own call (`frac(...)`) is not continuable.
    const continuable = ["letter", "ident", "str", "esc"].includes(first.k) && last === first;
    const next = this.peek();
    if (continuable && next?.k === "open" && last && last.e === next.s) {
      this.pos++;
      const save = this.pos;
      const g = this.group(next);
      // Only a closed group forms a unit; an unclosed opening is parsed on its own.
      if (g.kind === "delim") return juxt(p, [g]);
      this.pos = save - 1;
    }
    return p;
  }

  private peekNonSpace(): Tk | undefined {
    let i = this.pos;
    while (this.toks[i]?.k === "space") i++;
    return this.toks[i];
  }

  private primary(stop: (t: Tk) => boolean, closeStops: boolean, arg = false): MathNode | null {
    this.skipSpace();
    const t = this.peek();
    if (!t || stop(t)) return null;
    this.pos++;
    switch (t.k) {
      case "letter":
        return { kind: "ident", text: t.v };
      case "number":
        return { kind: "number", text: t.v };
      case "str": {
        const body = t.v.slice(1, -1);
        const text = unescapeString(body);
        if (text === "") return null;
        // The written form is kept when it is not the one the writer would produce (`\u{3b1}`).
        return escapeString(text) === body ? { kind: "text", text } : { kind: "text", text, raw: body };
      }
      case "comment":
        return this.opaque(t.s, t.e, "comment");
      case "code":
        return this.opaque(t.s, t.e, "embedded code");
      case "esc":
      case "short":
      case "char":
        return this.glyphNode(t.g ?? t.v, t);
      case "primes":
        return this.primeRun(t);
      case "open":
        return this.group(t);
      case "close":
        if (closeStops) {
          this.pos--;
          return null;
        }
        return this.glyphNode(t.g ?? t.v, t);
      case "ident":
        return this.identifier(t);
      case "amp":
      case "linebreak":
      case "sub":
      case "sup":
      case "slash":
        if (arg) {
          this.pos--;
          return null;
        }
        if (t.k === "sub" || t.k === "sup") return this.opaqueRest(t.s, stop, closeStops, "missing attachment");
        return this.opaque(t.s, t.e, "unexpected token");
      case "space":
        return null;
    }
  }

  private primeRun(t: Tk): MathNode {
    return { kind: "opaque", source: t.v, language: "typst", reason: "primes" };
  }

  private glyphNode(g: string, t: Tk): MathNode {
    if (/^\p{L}$/u.test(g)) return { kind: "ident", text: g };
    if (/^[0-9]$/.test(g)) return { kind: "number", text: g };
    if (g === "*") return { kind: "symbol", id: "op.ast" };
    const e = entryByGlyph(g);
    if (e) return this.entryNode(e);
    return this.opaque(t.s, t.e, `unknown symbol ${g}`);
  }

  private entryNode(e: CatalogEntry): MathNode {
    if (e.role === "bigop") return { kind: "bigop", op: e.id, lower: null, upper: null };
    if (e.role === "func") return { kind: "func", name: e.id };
    return { kind: "symbol", id: e.id };
  }

  /** A delimited group starting with an opening delimiter. */
  private group(open: Tk): MathNode {
    const save = this.pos;
    const body = this.parseRow(() => false, true);
    const close = this.peek();
    const leftId = OPENINGS.get(open.g ?? open.v) as string;
    if (close?.k === "close") {
      this.pos++;
      const rightId = CLOSINGS.get(close.g ?? close.v) as string;
      const node: MathNode = { kind: "delim", left: leftId, right: rightId, body };
      if (open.v === "(") this.bare.add(node);
      return node;
    }
    // Unclosed: the opening is an ordinary symbol; what follows is parsed normally.
    this.pos = save;
    return { kind: "symbol", id: leftId };
  }

  /** Raw argument text of a call at the current `(`; null when there is none. */
  private callArgs(): { inner: string; innerStart: number; end: number } | null {
    const t = this.peek();
    if (t?.k !== "open" || t.v !== "(") return null;
    let depth = 0;
    for (let p = this.pos; p < this.toks.length; p++) {
      const x = this.toks[p] as Tk;
      if (x.k === "open") depth++;
      else if (x.k === "close") {
        depth--;
        if (depth === 0) {
          this.pos = p + 1;
          return { inner: this.src.slice(t.e, x.s), innerStart: t.e, end: x.e };
        }
      }
    }
    return null;
  }

  private identifier(t: Tk): MathNode {
    const name = t.v;
    const next = this.peek();
    const isCall = next?.k === "open" && next.v === "(" && next.s === t.e;
    if (isCall) {
      const call = this.call(name, t);
      if (call) return call;
    }
    const aliased = TYPST_ALIASES[name];
    const e = aliased ? entryById(aliased) : entryByTypst(name);
    if (e && (e.role === "symbol" || e.role === "delim" || e.role === "bigop" || e.role === "func")) return this.entryNode(e);
    return this.opaque(t.s, t.e, `unknown name ${name}`);
  }

  /** Function calls; null when `name(` is juxtaposition (a symbol or operator followed by parentheses). */
  private call(name: string, t: Tk): MathNode | null {
    const save = this.pos;
    const args = this.callArgs();
    if (!args) return null;
    const opaque = () => this.opaque(t.s, args.end, `unsupported call ${name}`);
    const { rows } = splitArgs(args.inner);
    const flat = rows.length === 1 ? (rows[0] as string[]) : null;
    const named = (rows.flat() as string[]).some((a) => NAMED_ARG.test(a));
    const sub = (s: string) => parseTypstMath(s).row;
    const one = flat && flat.length === 1 && !named ? sub(flat[0] as string) : null;
    switch (name) {
      case "frac":
      case "binom":
        if (!flat || flat.length !== 2 || named) return opaque();
        return name === "frac"
          ? { kind: "frac", num: sub(flat[0] as string), den: sub(flat[1] as string) }
          : { kind: "frac", num: sub(flat[0] as string), den: sub(flat[1] as string), variant: "binom" };
      case "sqrt":
        return one ? { kind: "sqrt", body: one, index: null } : opaque();
      case "root":
        if (!flat || flat.length !== 2 || named) return opaque();
        return { kind: "sqrt", index: sub(flat[0] as string), body: sub(flat[1] as string) };
      case "abs":
      case "norm":
      case "floor":
      case "ceil": {
        if (!one) return opaque();
        const ids: Record<string, [string, string]> = {
          abs: ["delim.vert", "delim.vert"],
          norm: ["delim.Vert", "delim.Vert"],
          floor: ["delim.floor.l", "delim.floor.r"],
          ceil: ["delim.ceil.l", "delim.ceil.r"],
        };
        const [l, r] = ids[name] as [string, string];
        // Same tree as `lr(|x|)` and a template: Typst writes both as `abs(x)`.
        return { kind: "delim", left: l, right: r, body: one };
      }
      case "lr": {
        if (!one) return opaque();
        const only = one.children.length === 1 ? one.children[0] : undefined;
        if (only?.kind === "delim" && only.right !== "delim.none") return { kind: "delim", left: only.left, right: only.right, body: only.body };
        const first = one.children[0];
        const last = one.children[one.children.length - 1];
        const isDelim = (n: MathNode | undefined) => n?.kind === "symbol" && entryById(n.id)?.role === "delim";
        if (one.children.length >= 2 && isDelim(first) && isDelim(last) && first?.kind === "symbol" && last?.kind === "symbol") {
          return { kind: "delim", left: first.id, right: last.id, body: row(one.children.slice(1, -1)) };
        }
        return opaque();
      }
      case "op": {
        if (!flat || flat.length < 1 || flat.length > 2) return opaque();
        const m = /^\s*"([^"\\]*)"\s*$/.exec(flat[0] as string);
        if (!m) return opaque();
        const text = m[1] as string;
        if (flat.length === 2) {
          // Only the catalog's own `op("...", limits: #true)` operators (arg max, arg min).
          if (!/^\s*limits\s*:\s*#true\s*$/.test(flat[1] as string)) return opaque();
          const limited = entryByTypst(`op("${text}", limits: #true)`);
          return limited?.role === "func" ? { kind: "func", name: limited.id } : opaque();
        }
        if (named) return opaque();
        const e = entryByLatex(`\\operatorname{${text}}`);
        if (e?.role === "func") return { kind: "func", name: e.id };
        return { kind: "func", name: "func.operatorname", text };
      }
      case "attach": {
        // attach(base, b: sub, t: sup): written for multi-node bases.
        const all = rows.length === 1 ? (rows[0] as string[]) : null;
        if (!all || all.length < 2 || all.length > 3) return opaque();
        const [baseArg, ...rest] = all as [string, ...string[]];
        if (NAMED_ARG.test(baseArg)) return opaque();
        let subRow: MathRow | null = null;
        let supRow: MathRow | null = null;
        for (const a of rest) {
          const m = /^\s*(b|t)\s*:(?!=)([\s\S]*)$/.exec(a);
          if (!m) return opaque();
          if (m[1] === "b") subRow = sub(m[2] as string);
          else supRow = sub(m[2] as string);
        }
        if (!subRow && !supRow) return opaque();
        return { kind: "scripts", base: sub(baseArg), sub: subRow, sup: supRow };
      }
      case "mat":
      case "vec":
      case "cases":
        return this.gridCall(name, args.inner, opaque);
    }
    if (FONT_FUNCS.has(name) && one) {
      // Catalog symbols spelled with a style function (`upright(e)`, `bb(k)`) read back as the symbol.
      const letter = one.children.length === 1 ? one.children[0] : undefined;
      if (letter?.kind === "ident") {
        const symbol = entryByTypst(`${name}(${letter.text})`);
        if (symbol?.role === "symbol") return { kind: "symbol", id: symbol.id };
      }
      // bold(upright(x)) and upright(bold(x)) are LaTeX \mathbf.
      const inner = one.children.length === 1 ? one.children[0] : undefined;
      if ((name === "bold" || name === "upright") && inner?.kind === "font") {
        const pair = name === "bold" ? "font.rm" : "font.boldsymbol";
        if (inner.font === pair) return { kind: "font", font: "font.bf", body: inner.body };
      }
      const fontId = name === "bold" ? "font.boldsymbol" : (entryByTypst(name)?.id ?? "");
      if (fontId) return { kind: "font", font: fontId, body: one };
    }
    const accent = ACCENT_BY_TYPST.get(name);
    if (accent) return one ? { kind: "accent", accent, body: one } : opaque();
    const aliased = TYPST_ALIASES[name];
    const e = aliased ? entryById(aliased) : entryByTypst(name);
    if (e && (e.role === "symbol" || e.role === "delim") && ACCENT_CHARS.has(e.glyph)) {
      // Typst calls a symbol whose character is an accent as that accent (`arrow.r(x)`, `dot.op(x)`).
      const byGlyph = ACCENT_BY_GLYPH[e.glyph];
      return byGlyph && one ? { kind: "accent", accent: byGlyph, body: one } : opaque();
    }
    if (e && (e.role === "symbol" || e.role === "func" || e.role === "bigop" || e.role === "delim")) {
      // `sin(x)`, `alpha(x)`: juxtaposition with a parenthesized group.
      this.pos = save;
      return null;
    }
    return opaque();
  }

  private gridCall(name: string, inner: string, opaque: () => MathNode): MathNode {
    const { rows } = splitArgs(inner);
    let fence = "delim.paren";
    const content: string[][] = [];
    for (const r of rows) {
      const cells: string[] = [];
      for (const a of r) {
        const m = /^\s*([\p{L}_][\p{L}\p{N}_-]*)\s*:(?!=)\s*([\s\S]*?)\s*$/u.exec(a);
        if (m) {
          if (m[1] !== "delim" || name === "cases") return opaque();
          const value = m[2] as string;
          const fences: Record<string, string> = { '"("': "delim.paren", '"["': "delim.bracket", '"{"': "delim.brace", '"|"': "delim.vert", '"||"': "delim.Vert", "#none": "none" };
          const f = fences[value];
          if (!f) return opaque();
          fence = f;
          continue;
        }
        cells.push(a);
      }
      content.push(cells);
    }
    // A trailing comma or semicolon leaves an empty last argument.
    const clean = content.map((cells) => (cells.length > 1 && (cells[cells.length - 1] as string).trim() === "" ? cells.slice(0, -1) : cells)).filter((cells, i, all) => !(i === all.length - 1 && all.length > 1 && cells.length === 1 && (cells[0] as string).trim() === ""));
    const sub = (s: string) => parseTypstMath(s).row;
    if (name === "vec") {
      if (clean.length !== 1) return opaque();
      return { kind: "matrix", fence, rows: (clean[0] as string[]).map((c) => [sub(c)]), form: "vec" };
    }
    if (name === "mat") {
      return { kind: "matrix", fence, rows: clean.map((cells) => cells.map(sub)) };
    }
    // cases: one argument per row, `value & condition`.
    if (clean.length !== 1) return opaque();
    const caseRows: [MathRow, MathRow][] = [];
    for (const arg of clean[0] as string[]) {
      const parsed = parseTypstMath(arg).row;
      const only = parsed.children.length === 1 ? parsed.children[0] : undefined;
      if (only?.kind === "aligned" && only.bare) {
        if (only.rows.length !== 1 || (only.rows[0] as MathRow[]).length > 2) return opaque();
        const [v, c] = only.rows[0] as MathRow[];
        caseRows.push([v ?? row(), c ?? row()]);
      } else caseRows.push([parsed, row()]);
    }
    return { kind: "cases", rows: caseRows };
  }
}

/** Split multi-digit number nodes into one node per character (the tree's unit). */
function flattenNumbers(r: MathRow): MathRow {
  const children: MathNode[] = [];
  for (const n of r.children) {
    if (n.kind === "number" && n.text.length > 1) {
      for (const d of n.text) children.push({ kind: "number", text: d });
      continue;
    }
    const prev = children[children.length - 1];
    if (n.kind === "symbol" && n.id === "punct.period" && prev?.kind === "number" && /[0-9]/.test(prev.text)) {
      children.push({ kind: "number", text: "." });
      continue;
    }
    children.push(mapSlots(n, flattenNumbers));
  }
  return row(children);
}

function mapSlots(n: MathNode, f: (r: MathRow) => MathRow): MathNode {
  switch (n.kind) {
    case "frac":
      return { ...n, num: f(n.num), den: f(n.den) };
    case "sqrt":
      return { ...n, body: f(n.body), index: n.index ? f(n.index) : null };
    case "scripts":
      return { ...n, base: f(n.base), sub: n.sub ? f(n.sub) : null, sup: n.sup ? f(n.sup) : null };
    case "bigop":
      return { ...n, lower: n.lower ? f(n.lower) : null, upper: n.upper ? f(n.upper) : null };
    case "delim":
    case "accent":
    case "font":
      return { ...n, body: f(n.body) };
    case "matrix":
    case "aligned":
      return { ...n, rows: n.rows.map((cells) => cells.map(f)) };
    case "cases":
      return { ...n, rows: n.rows.map(([a, b]): [MathRow, MathRow] => [f(a), f(b)]) };
    default:
      return n;
  }
}

/** Primes lexed as opaque placeholders become prime symbols. */
function resolvePrimes(r: MathRow): MathRow {
  const children: MathNode[] = [];
  for (const n of r.children) {
    if (n.kind === "opaque" && n.reason === "primes") {
      for (let i = 0; i < n.source.length; i++) children.push({ kind: "symbol", id: "calc.prime" });
      continue;
    }
    children.push(mapSlots(n, resolvePrimes));
  }
  return row(children);
}

export function parseTypstMath(content: string): ParseResult {
  const parser = new TypstParser(content);
  const r = resolvePrimes(flattenNumbers(parser.parseContent()));
  return { row: r, complete: isComplete(r) };
}

// ================================================================ serializer

type Cls = "ord" | "bin" | "rel" | "op" | "open" | "close" | "punct";
interface Piece {
  text: string;
  cls: Cls;
  /** The piece ends with a multi-letter name (`alpha`, `arrow.r`). */
  name?: boolean;
  /** Node kind that produced the piece. */
  kind?: MathNode["kind"];
}

/** LaTeX spacing and numbering commands with a Typst equivalent ("" = nothing to write). */
const LATEX_SPACING_TO_TYPST: Record<string, string> = {
  "\\,": "thin",
  "\\:": "med",
  "\\>": "med",
  "\\;": "thick",
  "\\ ": "thick",
  "\\quad": "quad",
  "\\qquad": "wide",
  "\\!": "#h(-0.1667em)",
  "\\nonumber": "",
  "\\notag": "",
};
/** Typst spacing names and their LaTeX commands. */
export const TYPST_SPACING_TO_LATEX: Record<string, string> = { thin: "\\,", med: "\\:", thick: "\\;", quad: "\\quad", wide: "\\qquad" };

const LITERAL_ASCII = new Set(["+", "=", "<", ">", "!", ",", ";", ":", ".", "?", "|", "(", ")", "[", "]", "{", "}"]);
const SHORTHAND_PREFIXES = new Set(SHORTHANDS.flatMap(([k]) => [...Array(k.length).keys()].slice(1).map((n) => k.slice(0, n + 1))));

/**
 * Every form this writer emits reads back as the same tree (property.test.ts), except catalog
 * variants that Typst spells identically and that therefore read back as the first entry:
 * hslash → hbar (`planck`), smallsetminus → setminus (`without`), \implies / \impliedby / \iff →
 * the long double arrows, \prime → `'`, \vert / \Vert commands and \lvert…\rvert → `|` / `||`,
 * \colon → `:`, \widehat / \widetilde → hat / tilde, \overrightarrow → vec (`arrow`),
 * \bm → \boldsymbol (`bold`), \triangle / \triangledown → \bigtriangleup / \bigtriangledown.
 * Typst `lr(|x|)` and `abs(x)` are the same tree (written `abs(x)`).
 */
class TypstWriter {
  readonly unsupported: { what: string; reason: string }[] = [];
  /** Unpaired bracket symbols in the current row may be written literally (they balance). */
  private literalBrackets = true;

  private reject(what: string, reason: string): void {
    // One report per catalog id or kind; opaque source is reported per fragment.
    if (!this.unsupported.some((u) => u.what === what && (what !== "opaque" || u.reason === reason))) this.unsupported.push({ what, reason });
  }

  /** `inArgs`: inside function call arguments, where `,` and `;` separate arguments. */
  row(r: MathRow, script = false, inArgs = false): string {
    const saved = this.literalBrackets;
    this.literalBrackets = bracketsBalance(r);
    const pieces = r.children.map((n) => ({ ...this.node(n, inArgs), kind: n.kind })).filter((p) => p.text !== "");
    this.literalBrackets = saved;
    return joinTypst(pieces, script);
  }

  private symbolText(id: string, inArgs: boolean): Piece {
    const e = entryById(id);
    if (!e) {
      this.reject(id, "unknown catalog id");
      return { text: "", cls: "ord" };
    }
    const cls = classOf(id);
    if (id === "calc.prime") return { text: "'", cls: "ord" };
    // Typst pairs any opening with any closing delimiter: unpaired brackets are written
    // literally only when they balance in this row (`[0, 1)`), otherwise by name.
    const bracket = "()[]{}".includes(e.glyph);
    if (LITERAL_ASCII.has(e.glyph) && (e.role === "symbol" || id === "delim.vert" || (bracket && this.literalBrackets))) {
      if (inArgs && (e.glyph === "," || e.glyph === ";")) return { text: `\\${e.glyph}`, cls };
      return { text: e.glyph, cls };
    }
    const short = SHORTHAND_OUT.get(e.glyph);
    if (short && e.role !== "delim") return { text: short, cls };
    if (id === "delim.Vert" || id === "delim.Vert.cmd") return { text: "||", cls };
    if (e.typst) return { text: e.typst.name, cls, name: /^[\p{L}][\p{L}\p{N}.]*$/u.test(e.typst.name) && e.typst.name.length > 1 };
    // No verified Typst spelling: write the character (Typst accepts Unicode) but report it,
    // so that a draft holding it is not applied silently.
    this.reject(id, "no verified Typst spelling");
    if (e.glyph && [...e.glyph].length === 1) return { text: e.glyph, cls };
    return { text: "", cls };
    this.reject(id, "no Typst equivalent");
    return { text: "", cls };
  }

  /** Operand of `^`, `_`, or a slash fraction: atomic as is, otherwise parenthesized. */
  private operand(r: MathRow, inArgs: boolean): string {
    // An empty operand is the empty string (`x^""`), like empty arguments and cells.
    if (r.children.length === 0) return '""';
    const text = this.row(r, true, inArgs);
    return this.atomic(r) ? text : `(${text})`;
  }

  private atomic(r: MathRow): boolean {
    if (r.children.length === 0) return false;
    if (r.children.every((n) => n.kind === "number")) return /^[0-9]+(\.[0-9]+)?$/.test(r.children.map((n) => (n as { text: string }).text).join(""));
    // `f(x)` / `sin(x)`: a name directly followed by a group is one unit in Typst.
    if (r.children.length === 2) {
      const [a, b] = r.children as [MathNode, MathNode];
      // Only a group written with a bracket character joins the name (`f(x)`, not `f abs(x)`).
      const bracket = (id: string) => ["delim.paren.l", "delim.bracket.l", "delim.brace.l"].includes(id);
      // A function written as a call (`op("rank")`) is complete; only bare names group (`sin(x)`).
      const bareFunc = a.kind === "func" && a.name !== "func.operatorname" && /^[A-Za-z]+$/.test(entryById(a.name)?.typst?.name ?? "");
      return (bareFunc || (a.kind === "ident" && [...a.text].length === 1)) && b.kind === "delim" && bracket(b.left) && b.form !== "func";
    }
    if (r.children.length !== 1) return false;
    const n = r.children[0] as MathNode;
    switch (n.kind) {
      case "ident":
        return [...n.text].length === 1;
      case "symbol":
      case "func":
      case "text":
      case "sqrt":
      case "accent":
      case "font":
      case "matrix":
      case "cases":
        return true;
      case "frac":
        return n.variant !== "slash";
      case "delim":
        return !(n.left === "delim.paren.l" && n.right === "delim.paren.r" && n.form !== "func");
      default:
        return false;
    }
  }

  private scripts(sub: MathRow | null, sup: MathRow | null, inArgs: boolean): string {
    let s = "";
    if (sub) s += `_${this.operand(sub, inArgs)}`;
    if (sup) s += `^${this.operand(sup, inArgs)}`;
    return s;
  }

  private args(...rows: MathRow[]): string {
    return rows.map((r) => this.argText(r)).join(", ");
  }

  /** One call argument; `""` keeps `t: x` from being read as a named argument. */
  private argText(r: MathRow): string {
    const text = this.row(r, false, true);
    // An empty argument would be a missing argument: write the empty string.
    if (text === "") return '""';
    return NAMED_ARG.test(text) ? `""${text}` : text;
  }

  node(n: MathNode, inArgs: boolean): Piece {
    switch (n.kind) {
      case "ident":
        return { text: [...n.text].join(" "), cls: "ord" };
      case "number":
        return { text: n.text, cls: "ord" };
      case "symbol":
        return this.symbolText(n.id, inArgs);
      case "text":
        // An empty text renders nothing (and `""` is read back as an empty operand).
        if (n.text === "") return { text: "", cls: "ord" };
        return { text: `"${n.raw !== undefined && unescapeString(n.raw) === n.text ? n.raw : escapeString(n.text)}"`, cls: "ord" };
      case "frac":
        if (n.variant === "slash") return { text: `${this.operand(n.num, inArgs)} / ${this.operand(n.den, inArgs)}`, cls: "ord" };
        return { text: `${n.variant === "binom" ? "binom" : "frac"}(${this.args(n.num, n.den)})`, cls: "ord" };
      case "sqrt":
        return { text: n.index ? `root(${this.args(n.index, n.body)})` : `sqrt(${this.args(n.body)})`, cls: "ord" };
      case "scripts": {
        const only = n.base.children.length === 1 ? n.base.children[0] : undefined;
        let base: string;
        if (n.base.children.length === 0) base = '""';
        else if (n.base.children.every((c) => c.kind === "number")) base = this.row(n.base, false, inArgs);
        else if (!only && this.atomic(n.base)) base = this.row(n.base, false, inArgs);
        else if (only?.kind === "scripts" && nestsDirectly(only, n)) {
          // `x^a^b`: Typst nests a second attachment of the same kind.
          base = this.node(only, inArgs).text;
        } else if (!only || !(this.atomic(n.base) || only.kind === "delim" || (only.kind === "frac" && only.variant === "slash"))) {
          // Bases that Typst cannot take as they are: an explicit attach().
          const parts = [`${this.args(n.base)}`];
          if (n.sub) parts.push(`b: ${this.args(n.sub)}`);
          if (n.sup) parts.push(`t: ${this.args(n.sup)}`);
          return { text: `attach(${parts.join(", ")})`, cls: "ord" };
        } else if (only.kind === "frac" && only.variant === "slash") base = this.node({ ...only, variant: undefined }, inArgs).text;
        else {
          const saved = this.literalBrackets;
          this.literalBrackets = bracketsBalance(n.base);
          base = this.node(only, inArgs).text;
          this.literalBrackets = saved;
        }
        return { text: `${base}${this.scripts(n.sub, n.sup, inArgs)}`, cls: "ord" };
      }
      case "bigop": {
        const e = entryById(n.op);
        let text: string;
        if (e?.role === "func") text = this.funcText(n.op);
        else if (e?.typst) text = e.typst.name;
        else if (e?.glyph) text = e.glyph;
        else {
          this.reject(n.op, "no Typst equivalent");
          text = "";
        }
        return { text: `${text}${this.scripts(n.lower, n.upper, inArgs)}`, cls: "op", name: !n.lower && !n.upper };
      }
      case "func":
        if (n.name === "func.operatorname") return { text: `op("${(n.text ?? "").replace(/[\\"]/g, (c) => `\\${c}`)}")`, cls: "op" };
        return { text: this.funcText(n.name), cls: classOf(n.name) };
      case "delim":
        return { text: this.delim(n), cls: "ord" };
      case "accent": {
        const e = entryById(n.accent);
        if (!e?.typst) {
          this.reject(n.accent, "no Typst equivalent");
          return { text: this.row(n.body, false, inArgs), cls: "ord" };
        }
        return { text: `${e.typst.name}(${this.args(n.body)})`, cls: "ord" };
      }
      case "font": {
        if (n.font === "font.bf") return { text: `bold(upright(${this.args(n.body)}))`, cls: "ord" };
        const e = entryById(n.font);
        if (!e?.typst) {
          this.reject(n.font, "no Typst equivalent");
          return { text: this.row(n.body, false, inArgs), cls: "ord" };
        }
        return { text: `${e.typst.name}(${this.args(n.body)})`, cls: "ord" };
      }
      case "matrix": {
        const fences: Record<string, string> = { "delim.bracket": '"["', "delim.brace": '"{"', "delim.vert": '"|"', "delim.Vert": '"||"', none: "#none" };
        if (n.fence !== "delim.paren" && !fences[n.fence]) this.reject(n.fence, "unknown matrix fence");
        const delim = n.fence === "delim.paren" ? [] : [`delim: ${fences[n.fence] ?? '"("'}`];
        const cell = (r: MathRow) => (r.children.length ? this.argText(r) : '""');
        if (n.form === "vec" && n.rows.every((r) => r.length === 1)) return { text: `vec(${[...delim, ...n.rows.map((r) => cell(r[0] as MathRow))].join(", ")})`, cls: "ord" };
        const body = n.rows.map((cells) => cells.map(cell).join(", ")).join("; ");
        return { text: `mat(${[...delim, ...(body ? [body] : [])].join(", ")})`, cls: "ord" };
      }
      case "cases": {
        // An empty row is written as `""` so that it is not read back as a trailing comma.
        const rows = n.rows.map(([v, c]) => (isEmptyRow(c) ? this.argText(v) || '""' : `${this.argText(v)} & ${this.row(c, false, true)}`));
        return { text: `cases(${rows.join(", ")})`, cls: "ord" };
      }
      case "aligned":
        return { text: this.grid(n.rows, inArgs), cls: "ord" };
      case "opaque": {
        // A line comment must end its line (or it would swallow the closing `$`).
        if (n.language === "typst") {
          if (n.reason === "comment" && n.source.startsWith("//")) return { text: `${n.source}\n`, cls: "ord" };
          // A trailing backslash (a line break where none is allowed) must not escape what follows.
          if (/(^|[^\\])(\\\\)*\\$/.test(n.source)) return { text: `${n.source} `, cls: "ord" };
          // Source ending inside a line comment must end its line.
          return { text: endsInLineComment(n.source) ? `${n.source}\n` : n.source, cls: "ord" };
        }
        const spacing = LATEX_SPACING_TO_TYPST[n.source.trim()];
        if (spacing !== undefined) return { text: spacing, cls: "ord", name: spacing !== "" };
        this.reject("opaque", `LaTeX source cannot be written as Typst: ${n.source}`);
        return { text: "", cls: "ord" };
      }
    }
  }

  private funcText(id: string): string {
    const e = entryById(id);
    if (e?.typst) return e.typst.name;
    const m = e?.latex?.command.match(/^\\operatorname\{([^}]*)\}$/);
    if (m) return `op("${m[1]}")`;
    if (e?.latex && /^\\[A-Za-z]+$/.test(e.latex.command)) return `op("${e.latex.command.slice(1)}")`;
    this.reject(id, "no Typst equivalent");
    return "";
  }

  private delim(n: Extract<MathNode, { kind: "delim" }>): string {
    const body = this.row(n.body);
    const pair = `${n.left}|${n.right}`;
    const funcs: Record<string, string> = {
      "delim.vert|delim.vert": "abs",
      "delim.vert.l|delim.vert.r": "abs",
      "delim.vert.cmd|delim.vert.cmd": "abs",
      "delim.Vert|delim.Vert": "norm",
      "delim.Vert.cmd|delim.Vert.cmd": "norm",
      "delim.Vert.l|delim.Vert.r": "norm",
      "delim.floor.l|delim.floor.r": "floor",
      "delim.ceil.l|delim.ceil.r": "ceil",
    };
    const fn = funcs[pair];
    if (fn) return `${fn}(${this.row(n.body, false, true)})`;
    const ascii: Record<string, string> = {
      "delim.paren.l": "(",
      "delim.bracket.l": "[",
      "delim.brace.l": "{",
      "delim.paren.r": ")",
      "delim.bracket.r": "]",
      "delim.brace.r": "}",
    };
    const l = ascii[n.left];
    const r = ascii[n.right];
    if (l && r) return glue(glue(l, body), r);
    const side = (id: string) => {
      if (id === "delim.none") return "";
      const e = entryById(id);
      if (ascii[id]) return ascii[id] as string;
      if (e?.typst) return e.typst.name;
      this.reject(id, "no Typst equivalent");
      return e?.glyph ?? "";
    };
    // Inside `lr(...)` the body is a call argument: its commas must be escaped.
    const parts = [side(n.left), this.row(n.body, false, true), side(n.right)].filter((p) => p !== "");
    return `lr(${parts.join(" ")})`;
  }

  private grid(rows: MathRow[][], inArgs: boolean): string {
    return rows
      .map((cells) =>
        cells
          .map((c, i) => {
            const text = this.row(c, false, inArgs);
            if (i === 0) return text;
            const first = c.children[0];
            const tight = !text || (first?.kind === "symbol" && classOf(first.id) === "rel");
            return `${tight ? "&" : "& "}${text}`;
          })
          .join(" ")
          .replace(/^ /, ""),
      )
      .map((text, i, all) => (i > 0 && i === all.length - 1 && text === "" ? '""' : text))
      .join(" \\\n");
  }
}

/** A named call argument (`delim: ...`, `_t: ...`): Typst identifiers may start with `_`. */
const NAMED_ARG = /^\s*[\p{L}_][\p{L}\p{N}_-]*\s*:(?!=)/u;

/**
 * Whether `inner` followed directly by `outer`'s attachments reads back as the same nesting.
 * Typst chains `^` with `_` into one attachment: `x^a_b` is not `(x^a)_b`.
 */
function nestsDirectly(inner: Extract<MathNode, { kind: "scripts" }>, outer: Extract<MathNode, { kind: "scripts" }>): boolean {
  if (inner.sub && inner.sup) return true;
  const single = (n: typeof inner) => (n.sub && !n.sup ? "sub" : n.sup && !n.sub ? "sup" : null);
  const a = single(inner);
  return a !== null && single(outer) === a;
}

/** Typst string literal bodies: `\\\\`, `\\"`, `\\n`, `\\r`, `\\t`, `\\u{...}`. */
function unescapeString(body: string): string {
  return body.replace(/\\(u\{([0-9A-Fa-f]+)\}|.)/gs, (m, e: string, hex: string | undefined) => {
    if (hex !== undefined) {
      const cp = parseInt(hex, 16);
      return cp <= 0x10ffff ? String.fromCodePoint(cp) : m;
    }
    return e === "n" ? "\n" : e === "r" ? "\r" : e === "t" ? "\t" : e;
  });
}

function escapeString(text: string): string {
  return text.replace(/[\\"\n\r\t]/g, (c) => (c === "\n" ? "\\n" : c === "\r" ? "\\r" : c === "\t" ? "\\t" : `\\${c}`));
}

/** Accent functions by Typst name (first catalog entry wins: `hat` is \hat, not \widehat). */
const ACCENT_BY_TYPST = new Map<string, string>();
for (const e of allEntries()) if (e.role === "accent" && e.typst && !ACCENT_BY_TYPST.has(e.typst.name)) ACCENT_BY_TYPST.set(e.typst.name, e.id);

/** Characters Typst accepts as accents (typst-library math/accent.rs) and their catalog accents. */
const ACCENT_BY_GLYPH: Record<string, string> = {
  "`": "accent.grave",
  "´": "accent.acute",
  "^": "accent.hat",
  "ˆ": "accent.hat",
  "~": "accent.tilde",
  "∼": "accent.tilde",
  "˜": "accent.tilde",
  "¯": "accent.bar",
  "˘": "accent.breve",
  ".": "accent.dot",
  "˙": "accent.dot",
  "⋅": "accent.dot",
  "¨": "accent.ddot",
  "∘": "accent.mathring",
  "○": "accent.mathring",
  "ˇ": "accent.check",
  "←": "accent.overleftarrow",
  "→": "accent.vec",
  "⟶": "accent.vec",
  "↔": "accent.overleftrightarrow",
  "⟷": "accent.overleftrightarrow",
};
const ACCENT_CHARS = new Set([...Object.keys(ACCENT_BY_GLYPH), "-", "–", "‾", "−", "˝", "↼", "⇀"]);

/** Concatenate, separating characters that would otherwise form a shorthand (`[|`, `|]`, `||`). */
function glue(a: string, b: string): string {
  if (!a || !b) return a + b;
  return SHORTHAND_PREFIXES.has(a.slice(-1) + b.slice(0, 1)) ? `${a} ${b}` : a + b;
}

/** Whether the last line of Typst source ends inside a `//` comment (outside strings). */
function endsInLineComment(source: string): boolean {
  if (source.endsWith("\n")) return false;
  const line = source.slice(source.lastIndexOf("\n") + 1);
  let inString = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === "\\") i++;
    else if (c === '"') inString = !inString;
    else if (!inString && c === "/" && line[i + 1] === "/") return true;
  }
  return false;
}

/** Whether the bracket symbols of a row pair up (any opening with the next closing). */
function bracketsBalance(r: MathRow): boolean {
  let depth = 0;
  for (const n of r.children) {
    if (n.kind !== "symbol") continue;
    const glyph = entryById(n.id)?.glyph ?? "";
    if (OPENINGS.has(glyph)) depth++;
    else if (CLOSINGS.has(glyph)) {
      depth--;
      if (depth < 0) return false;
    }
  }
  return depth === 0;
}

const OPENING_START = /^(\[\||[([{⟨⌈⌊⟦])/u;

function joinTypst(pieces: Piece[], script: boolean): string {
  const cls: Cls[] = pieces.map((p, i) => {
    if (p.cls !== "bin") return p.cls;
    const prev = i > 0 ? (pieces[i - 1] as Piece).cls : undefined;
    return prev === undefined || ["bin", "rel", "open", "punct", "op"].includes(prev) ? "ord" : "bin";
  });
  let out = "";
  pieces.forEach((p, i) => {
    if (i > 0) {
      const prev = pieces[i - 1] as Piece;
      const a = cls[i - 1] as Cls;
      const b = cls[i] as Cls;
      let sep = "";
      if (!script && (a === "bin" || b === "bin" || a === "rel" || b === "rel" || a === "punct")) sep = " ";
      if (sep === "") {
        const last = prev.text.slice(-1);
        const first = p.text.slice(0, 1);
        // A continuable token (letter, name, string, prime) directly before a delimiter groups
        // with it as one operand; only a plain group after a simple atom may stay attached.
        // A name (letters, then letters or digits) must not run into the next letter or digit.
        const tail = /[\p{L}\p{N}]+$/u.exec(prev.text)?.[0] ?? "";
        // Escapes (`\;`) are continuable too.
        const continuable = last === '"' || /^\p{L}/u.test(tail) || /\\\S$/.test(prev.text);
        if (/^\p{L}/u.test(tail) && /[\p{L}\p{N}]/u.test(first)) sep = " ";
        else if (continuable && OPENING_START.test(p.text)) {
          const simple = (prev.kind === "ident" || prev.kind === "text" || prev.kind === "func") && !prev.name;
          if (!(simple && p.kind === "delim")) sep = " ";
        } else if (prev.name && first === ".") sep = " ";
        // Digits of different pieces must not form one number.
        else if (/\p{N}/u.test(last) && /\p{N}/u.test(first) && !(prev.kind === "number" && p.kind === "number")) sep = " ";
        else if (SHORTHAND_PREFIXES.has(last + first) || (last === "/" && (first === "/" || first === "*"))) sep = " ";
        else if (!script && a === "op" && /[_^]/.test(prev.text)) sep = " ";
        else if ((prev.kind === "scripts" || prev.kind === "bigop" || prev.kind === "frac") && /[\p{L}\p{N}.]/u.test(first)) sep = " ";
        else if (!script && /[\p{L}\p{N}"]/u.test(first) && (/[_^](\p{N}+|\p{L})$|[)"]$/u.test(prev.text) || a === "op")) sep = " ";
        else if (!script && first === '"' && /[\p{L}\p{N}]/u.test(last)) sep = " ";
      }
      // Embedded code continues with a directly following call, index, or field access.
      if (sep === "" && prev.text.startsWith("#") && prev.kind === "opaque" && /^[([.]/.test(p.text)) sep = " ";
      // A following `!` or prime would bind to a fraction's denominator or a script operand.
      if (sep === "" && /^[!']/.test(p.text) && (prev.kind === "frac" || prev.kind === "scripts" || prev.kind === "bigop")) sep = " ";
      if (prev.text.endsWith("\n")) sep = "";
      out += sep;
    }
    out += p.text;
  });
  return out;
}

export function serializeTypstMath(r: MathRow): SerializeResult {
  const w = new TypstWriter();
  const source = w.row(r);
  return { source, requirements: [], unsupported: w.unsupported };
}

// ================================================================ adapter

export const typstAdapter: MathLanguageAdapter = {
  language: "typst",
  equations: (text) => scanTypst(text).equations,
  contextAt: typstContext,
  parse: parseTypstMath,
  serialize: serializeTypstMath,
  wrap: (content, display) => (display ? `$ ${content} $` : `$${content}$`),
  satisfied: () => true,
  addRequirement: () => null,
};
