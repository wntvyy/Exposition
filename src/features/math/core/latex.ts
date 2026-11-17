// LaTeX math: equation locator, parser into the math tree, and serializer.
// The Markdown adapter reuses the parser and serializer (its content is a
// LaTeX subset) with `dialect: "markdown"`.
import { classOf, entryById, entryByGlyph, entryByLatex } from "./catalog";
import { isComplete, isEmptyRow, row } from "./tree";
import type {
  CatalogEntry,
  EquationRange,
  MathLanguage,
  MathLanguageAdapter,
  MathNode,
  MathRow,
  ParseResult,
  Requirement,
  SerializeResult,
  SourceContext,
} from "./types";

// ================================================================ locator

/** Environments whose body is display mathematics. */
export const LATEX_MATH_ENVS = new Set([
  "equation",
  "equation*",
  "align",
  "align*",
  "gather",
  "gather*",
  "multline",
  "multline*",
  "flalign",
  "flalign*",
  "displaymath",
  "math",
  "eqnarray",
  "eqnarray*",
  "alignat",
  "alignat*",
  "xalignat",
  "xalignat*",
  "xxalignat",
]);
const VERBATIM_ENVS = new Set(["verbatim", "verbatim*", "lstlisting", "minted", "comment", "Verbatim", "BVerbatim", "LVerbatim"]);

export interface Region {
  start: number;
  end: number;
  reason: string;
  /** Whether the offsets exactly at `start` / `end` are inside the region. */
  startIn?: boolean;
  endIn?: boolean;
}

interface LatexScan {
  equations: EquationRange[];
  unavailable: Region[];
}

const isLetter = (c: string | undefined) => c !== undefined && /[A-Za-z]/.test(c);

/** Read `{name}` at `i` (after optional spaces); returns the name and the end offset. */
function readBraced(text: string, i: number): { value: string; end: number } | null {
  let j = i;
  while (text[j] === " ") j++;
  if (text[j] !== "{") return null;
  const close = text.indexOf("}", j + 1);
  if (close < 0) return null;
  return { value: text.slice(j + 1, close), end: close + 1 };
}

/** End of a `{...}` argument read verbatim (balanced braces, no comments); null when absent. */
function readVerbatimGroup(text: string, i: number): number | null {
  let j = i;
  while (text[j] === " ") j++;
  if (text[j] !== "{") return null;
  let depth = 0;
  for (; j < text.length; j++) {
    if (text[j] === "\\") j++;
    else if (text[j] === "{") depth++;
    else if (text[j] === "}" && --depth === 0) return j + 1;
  }
  return null;
}

/** Skip a control sequence starting at the backslash `i`; returns the offset after it. */
function skipControl(text: string, i: number): number {
  if (isLetter(text[i + 1])) {
    let j = i + 1;
    while (isLetter(text[j])) j++;
    return j;
  }
  return Math.min(text.length, i + 2);
}

/** Find the end of math content starting at `i`, stopping at `closer` (outside comments and escapes). */
function findMathClose(text: string, i: number, closer: "$" | "$$" | "\\)" | "\\]"): number {
  let j = i;
  // Brace depth: a closer inside a group (e.g. `$` of math nested in `\text{...}`) is not ours.
  let depth = 0;
  while (j < text.length) {
    const c = text[j];
    if (c === "%") {
      const nl = text.indexOf("\n", j);
      j = nl < 0 ? text.length : nl + 1;
      continue;
    }
    if (c === "{") depth++;
    else if (c === "}") depth = Math.max(0, depth - 1);
    if (c === "\\") {
      if (depth === 0 && closer.startsWith("\\") && text.startsWith(closer, j)) return j;
      j = skipControl(text, j);
      continue;
    }
    if (c === "$" && depth === 0) {
      if (closer === "$") return j;
      if (closer === "$$") return text.startsWith("$$", j) ? j : -1;
      // A stray dollar inside \( \) or \[ \] is an error; keep scanning.
    }
    j++;
  }
  return -1;
}

/** Find `\end{env}` matching an environment whose body starts at `i`. */
function findEnvEnd(text: string, i: number, env: string): { start: number; end: number } | null {
  let depth = 0;
  let j = i;
  while (j < text.length) {
    const c = text[j];
    if (c === "%") {
      const nl = text.indexOf("\n", j);
      j = nl < 0 ? text.length : nl + 1;
      continue;
    }
    if (c === "\\") {
      const after = skipControl(text, j);
      const word = text.slice(j + 1, after);
      if (word === "begin" || word === "end") {
        const name = readBraced(text, after);
        if (name && name.value === env) {
          if (word === "begin") depth++;
          else if (depth === 0) return { start: j, end: name.end };
          else depth--;
          j = name.end;
          continue;
        }
      }
      j = after;
      continue;
    }
    j++;
  }
  return null;
}

/** Find the end of a verbatim-like environment (no nesting, no comment handling). */
function findVerbatimEnd(text: string, i: number, env: string): number {
  const marker = `\\end{${env}}`;
  const at = text.indexOf(marker, i);
  return at < 0 ? text.length : at + marker.length;
}

export function scanLatex(text: string): LatexScan {
  const equations: EquationRange[] = [];
  const unavailable: Region[] = [];
  // The preamble (everything before \begin{document}) is unavailable when a document environment exists.
  let bodyStart = 0;
  let bodyEnd = text.length;
  {
    const begin = findDocumentMarker(text, "begin");
    if (begin) {
      bodyStart = begin.end;
      unavailable.push({ start: 0, end: begin.end, reason: "preamble", startIn: true });
      const end = findDocumentMarker(text, "end", begin.end);
      if (end) {
        bodyEnd = end.start;
        unavailable.push({ start: end.start, end: text.length, reason: "after \\end{document}", endIn: true });
      }
    }
  }
  let i = bodyStart;
  while (i < bodyEnd) {
    const c = text[i];
    if (c === "%") {
      const nl = text.indexOf("\n", i);
      const end = nl < 0 ? text.length : nl;
      unavailable.push({ start: i, end, reason: "comment", endIn: true });
      i = end;
      continue;
    }
    if (c === "$") {
      if (text[i + 1] === "$") {
        const close = findMathClose(text, i + 2, "$$");
        if (close < 0 || close > bodyEnd) {
          i += 2;
          continue;
        }
        equations.push({ start: i, end: close + 2, contentStart: i + 2, contentEnd: close, display: true, style: "$$" });
        i = close + 2;
        continue;
      }
      const close = findMathClose(text, i + 1, "$");
      if (close < 0 || close > bodyEnd) {
        i++;
        continue;
      }
      equations.push({ start: i, end: close + 1, contentStart: i + 1, contentEnd: close, display: false, style: "$" });
      i = close + 1;
      continue;
    }
    if (c !== "\\") {
      i++;
      continue;
    }
    const next = text[i + 1];
    if (next === "(" || next === "[") {
      const closer = next === "(" ? "\\)" : "\\]";
      const close = findMathClose(text, i + 2, closer);
      if (close < 0 || close > bodyEnd) {
        i += 2;
        continue;
      }
      equations.push({
        start: i,
        end: close + 2,
        contentStart: i + 2,
        contentEnd: close,
        display: next === "[",
        style: next === "(" ? "\\(" : "\\[",
      });
      i = close + 2;
      continue;
    }
    const after = skipControl(text, i);
    const word = text.slice(i + 1, after);
    if (word === "url" || word === "href" || word === "path") {
      // URL arguments are read verbatim: `%` and `#` there are not comments.
      const arg = readVerbatimGroup(text, after);
      if (arg) {
        i = arg;
        continue;
      }
    }
    if (word === "verb" || word === "lstinline") {
      let j = after;
      if (text[j] === "*") j++;
      const delim = text[j];
      if (delim === undefined) {
        i = j;
        continue;
      }
      const closeDelim = delim === "{" ? "}" : delim;
      const close = text.indexOf(closeDelim, j + 1);
      const end = close < 0 ? text.length : close + 1;
      unavailable.push({ start: i, end, reason: "verbatim" });
      i = end;
      continue;
    }
    if (word === "begin") {
      const name = readBraced(text, after);
      if (name && VERBATIM_ENVS.has(name.value)) {
        const end = findVerbatimEnd(text, name.end, name.value);
        unavailable.push({ start: i, end, reason: "verbatim" });
        i = end;
        continue;
      }
      if (name && LATEX_MATH_ENVS.has(name.value)) {
        // alignat-style environments take a column count `{n}`; it stays outside the content.
        const columns = /alignat/.test(name.value) ? readBraced(text, name.end) : null;
        const bodyStart = columns ? columns.end : name.end;
        const end = findEnvEnd(text, bodyStart, name.value);
        if (end && end.end <= bodyEnd) {
          equations.push({
            start: i,
            end: end.end,
            contentStart: bodyStart,
            contentEnd: end.start,
            display: name.value !== "math",
            style: name.value,
          });
          i = end.end;
          continue;
        }
      }
      i = name ? name.end : after;
      continue;
    }
    i = after;
  }
  return { equations, unavailable };
}

function findDocumentMarker(text: string, word: "begin" | "end", from = 0): { start: number; end: number } | null {
  let i = from;
  while (i < text.length) {
    const c = text[i];
    if (c === "%") {
      const nl = text.indexOf("\n", i);
      i = nl < 0 ? text.length : nl + 1;
      continue;
    }
    if (c === "\\") {
      const after = skipControl(text, i);
      if (text.slice(i + 1, after) === word) {
        const name = readBraced(text, after);
        if (name && name.value === "document") return { start: i, end: name.end };
      }
      i = after;
      continue;
    }
    i++;
  }
  return null;
}

/** Shared context computation from equations and unavailable regions. */
export function contextFrom(equations: EquationRange[], unavailable: Region[], offset: number): SourceContext {
  for (const eq of equations) {
    if (offset >= eq.contentStart && offset <= eq.contentEnd) return { kind: "math", equation: eq };
    if (offset > eq.start && offset < eq.end) return { kind: "unavailable", reason: "equation delimiter" };
  }
  for (const r of unavailable) {
    const afterStart = offset > r.start || (r.startIn === true && offset === r.start);
    const beforeEnd = offset < r.end || (r.endIn === true && offset === r.end);
    if (afterStart && beforeEnd) return { kind: "unavailable", reason: r.reason };
  }
  return { kind: "text" };
}

// ================================================================ parser

type TokKind = "cmd" | "sym" | "char" | "open" | "close" | "sup" | "sub" | "amp" | "comment" | "space";
interface Tok {
  k: TokKind;
  /** Source text of the token (`\alpha`, `\,`, `x`, `{`). */
  v: string;
  s: number;
  e: number;
}

function tokenize(src: string, comments: boolean): Tok[] {
  const out: Tok[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i] as string;
    const start = i;
    if (c === "\\") {
      if (isLetter(src[i + 1])) {
        let j = i + 1;
        while (isLetter(src[j])) j++;
        out.push({ k: "cmd", v: src.slice(i, j), s: start, e: j });
        i = j;
      } else {
        const cp = src.codePointAt(i + 1);
        const len = cp === undefined ? 0 : cp > 0xffff ? 2 : 1;
        out.push({ k: "sym", v: src.slice(i, i + 1 + len), s: start, e: i + 1 + len });
        i += 1 + len;
      }
      continue;
    }
    if (/\s/.test(c)) {
      let j = i;
      while (j < src.length && /\s/.test(src[j] as string)) j++;
      out.push({ k: "space", v: src.slice(i, j), s: start, e: j });
      i = j;
      continue;
    }
    if (c === "%" && comments) {
      let j = src.indexOf("\n", i);
      if (j < 0) j = src.length;
      out.push({ k: "comment", v: src.slice(i, j), s: start, e: j });
      i = j;
      continue;
    }
    const kinds: Record<string, TokKind> = { "{": "open", "}": "close", "^": "sup", _: "sub", "&": "amp" };
    const cp = src.codePointAt(i) as number;
    const len = cp > 0xffff ? 2 : 1;
    out.push({ k: kinds[c] ?? "char", v: src.slice(i, i + len), s: start, e: i + len });
    i += len;
  }
  return out;
}

/** Functions whose scripts are limits (`\lim_{x \to 0}` becomes a bigop). */
const LIMIT_FUNCS = new Set([
  "func.lim",
  "func.limsup",
  "func.liminf",
  "func.max",
  "func.min",
  "func.sup",
  "func.inf",
  "func.det",
  "func.gcd",
  "func.Pr",
  "func.argmax",
  "func.argmin",
]);
export const isLimitFunc = (id: string) => LIMIT_FUNCS.has(id);

const MATRIX_ENVS: Record<string, string> = {
  matrix: "none",
  pmatrix: "delim.paren",
  bmatrix: "delim.bracket",
  Bmatrix: "delim.brace",
  vmatrix: "delim.vert",
  Vmatrix: "delim.Vert",
};
const FENCE_ENV: Record<string, string> = Object.fromEntries(Object.entries(MATRIX_ENVS).map(([k, v]) => [v, k]));
const ALIGNED_ENVS = new Set(["aligned", "gathered", "split"]);
const TEXT_CMDS = new Set(["\\text", "\\textrm", "\\textnormal", "\\mbox", "\\textit", "\\textbf"]);
const FRAC_CMDS: Record<string, "dfrac" | "tfrac" | "cfrac" | "binom" | undefined> = {
  "\\frac": undefined,
  "\\dfrac": "dfrac",
  "\\tfrac": "tfrac",
  "\\cfrac": "cfrac",
  "\\binom": "binom",
};
/** Plain (fixed-size) delimiter pairs recognized as `delim` nodes. */
const PLAIN_PAIRS: Record<string, string> = {
  "(": ")",
  "[": "]",
  "\\{": "\\}",
  "\\lbrace": "\\rbrace",
  "\\langle": "\\rangle",
  "\\lfloor": "\\rfloor",
  "\\lceil": "\\rceil",
  "\\lvert": "\\rvert",
  "\\lVert": "\\rVert",
};
/** Commands that never take arguments and stay opaque (spacing, styles, numbering). */
const OPAQUE_ONE_TOKEN = new Set([
  "\\quad",
  "\\qquad",
  "\\nonumber",
  "\\notag",
  "\\displaystyle",
  "\\textstyle",
  "\\scriptstyle",
  "\\scriptscriptstyle",
  "\\limits",
  "\\nolimits",
  "\\enspace",
  "\\thinspace",
  "\\negthinspace",
  "\\medspace",
  "\\thickspace",
  "\\hfill",
  "\\allowbreak",
]);
const SIZE_CMDS = new Set([
  "\\big",
  "\\Big",
  "\\bigg",
  "\\Bigg",
  "\\bigl",
  "\\bigr",
  "\\Bigl",
  "\\Bigr",
  "\\biggl",
  "\\biggr",
  "\\Biggl",
  "\\Biggr",
  "\\bigm",
  "\\Bigm",
  "\\middle",
]);

type Stop = (t: Tok) => boolean;

/** Non-delimiter entries LaTeX accepts after `\left` / `\right`. */
const SIZED_EXTRA = new Set(["op.slash", "arrow.t", "arrow.b", "arrow.tb", "arrow.t.double", "arrow.b.double", "arrow.tb.double"]);

class LatexParser {
  private pos = 0;
  /** Set for exactly the next item: a single-token macro argument, where `(` is one character, not a pair. */
  private singleToken = false;
  /** Number of structured environments being parsed (a `\end` only ends one of those). */
  private envDepth = 0;
  /** Source offset of the `{` of scripts nodes whose base is a group. */
  private readonly groupStart = new WeakMap<MathNode, number>();
  private readonly toks: Tok[];
  constructor(
    private readonly src: string,
    private readonly language: MathLanguage,
  ) {
    this.toks = tokenize(src, language !== "markdown");
  }

  /** Whether `{` and `}` pair up in the whole content. */
  bracesBalanced(): boolean {
    let depth = 0;
    for (const t of this.toks) {
      if (t.k === "open") depth++;
      else if (t.k === "close" && --depth < 0) return false;
    }
    return depth === 0;
  }

  parseContent(): MathRow {
    // Unbalanced braces cannot be structured safely: the content stays verbatim.
    if (!this.bracesBalanced()) return row(this.src.trim() ? [this.wholeOpaque("unbalanced braces")] : []);
    const rows = this.parseCells(() => false);
    if (rows.length === 1 && rows[0]?.length === 1) return rows[0][0] as MathRow;
    return row([{ kind: "aligned", rows, bare: true }]);
  }

  // ---------------------------------------------------------------- helpers

  private peek(): Tok | undefined {
    return this.toks[this.pos];
  }

  private skipSpace(): void {
    while (this.toks[this.pos]?.k === "space") this.pos++;
  }

  /**
   * An incomplete construct (missing argument) keeps the rest of its row verbatim, so that
   * nothing written after it can become its argument when the row is serialized again.
   */
  private opaqueRest(start: number, stop: Stop, reason: string): MathNode {
    let depth = 0;
    let envs = 0;
    while (this.pos < this.toks.length) {
      const t = this.toks[this.pos] as Tok;
      const atEdge = depth === 0 && envs === 0;
      if (depth === 0 && (t.k === "close" || stop(t))) break;
      if (atEdge && (t.k === "amp" || this.isRowBreak(t) || (this.envDepth > 0 && this.isEnd(t)))) break;
      if (t.k === "open") depth++;
      if (t.k === "close") depth--;
      if (t.k === "cmd" && t.v === "\\begin") envs++;
      if (this.isEnd(t)) envs--;
      this.pos++;
    }
    return this.opaque(start, this.lastEnd(start), reason);
  }

  /** End offset of the last consumed token that is not white space. */
  private lastEnd(fallback: number): number {
    for (let p = this.pos - 1; p >= 0; p--) {
      const t = this.toks[p] as Tok;
      if (t.k !== "space") return Math.max(t.e, fallback);
    }
    return fallback;
  }

  /** Next non-space token without consuming. */
  private peekNonSpace(): Tok | undefined {
    let p = this.pos;
    while (this.toks[p]?.k === "space") p++;
    return this.toks[p];
  }

  /** The whole content as one opaque node, without surrounding white space (the layout keeps it). */
  private wholeOpaque(reason: string): MathNode {
    const start = this.src.length - this.src.trimStart().length;
    return this.opaque(start, this.src.trimEnd().length, reason);
  }

  private opaque(start: number, end: number, reason: string): MathNode {
    return { kind: "opaque", source: this.src.slice(start, end), language: this.language, reason };
  }

  private isRowBreak(t: Tok): boolean {
    return t.k === "sym" && t.v === "\\\\";
  }

  private isEnd(t: Tok): boolean {
    return t.k === "cmd" && t.v === "\\end";
  }

  /** Rows of cells separated by `&` and `\\`, until `stop` (not consumed) or the end. */
  private parseCells(stop: Stop): MathRow[][] {
    const rows: MathRow[][] = [[]];
    let rowStart = -1;
    let modifier: MathNode | null = null;
    for (;;) {
      let cell = this.parseRow((t) => t.k === "amp" || this.isRowBreak(t) || stop(t), false);
      const current = rows[rows.length - 1] as MathRow[];
      if (modifier && current.length === 0) cell = row([modifier, ...cell.children]);
      modifier = null;
      current.push(cell);
      const t = this.peek();
      if (!t || stop(t)) break;
      this.pos++;
      if (t.k === "amp") continue;
      // Row break; `\\*` and `\\[len]` written directly after it belong to it.
      modifier = this.rowBreakModifier(t.e);
      rows.push([]);
      rowStart = this.pos;
    }
    // A final `\\` followed by nothing ends the last row instead of starting an empty one.
    const blank = rowStart >= 0 && this.toks.slice(rowStart, this.pos).every((t) => t.k === "space");
    return blank ? dropTrailingEmptyRow(rows) : rows;
  }

  /** `*` and/or `[len]` directly after a `\\\\` (no space, as amsmath reads them): an opaque modifier. */
  private rowBreakModifier(from: number): MathNode | null {
    let p = this.pos;
    let end = from;
    const at = (k: number, v: string) => {
      const x = this.toks[k];
      return x?.k === "char" && x.v === v && x.s === end;
    };
    if (at(p, "*")) {
      end = (this.toks[p] as Tok).e;
      p++;
    }
    if (at(p, "[")) {
      let depth = 0;
      for (let q = p + 1; q < this.toks.length; q++) {
        const x = this.toks[q] as Tok;
        if (x.k === "open") depth++;
        else if (x.k === "close") depth--;
        else if (depth === 0 && x.k === "char" && x.v === "]") {
          end = x.e;
          p = q + 1;
          break;
        }
        if (depth < 0) break;
      }
    }
    if (end === from) return null;
    this.pos = p;
    return this.opaque(from, end, ROW_BREAK_MODIFIER);
  }

  /** Parse nodes until `stop` matches (not consumed), a `}` closing the group (when `inGroup`), or the end. */
  private parseRow(stop: Stop, inGroup = true): MathRow {
    const nodes: MathNode[] = [];
    for (;;) {
      this.skipSpace();
      const t = this.peek();
      if (!t || stop(t)) break;
      if (t.k === "close" && inGroup) break;
      this.parseItem(nodes, stop);
    }
    return row(nodes);
  }

  /** Parse one item and append it (or modify the previous node for scripts). */
  private parseItem(nodes: MathNode[], stop: Stop): void {
    const t = this.peek() as Tok;
    const single = this.singleToken;
    this.singleToken = false;
    switch (t.k) {
      case "sup":
      case "sub":
        this.pos++;
        this.attachScript(nodes, t, stop);
        return;
      case "open":
        this.parseGroupItem(nodes);
        return;
      case "close":
        this.pos++;
        nodes.push(this.opaque(t.s, t.e, "unbalanced `}`"));
        return;
      case "amp":
        this.pos++;
        nodes.push(this.opaque(t.s, t.e, "alignment point outside an alignment"));
        return;
      case "comment":
        this.pos++;
        nodes.push(this.opaque(t.s, t.e, "comment"));
        return;
      case "char":
        this.pos++;
        this.parseChar(nodes, t, stop, single);
        return;
      case "sym":
        this.pos++;
        this.parseControlSymbol(nodes, t, stop, single);
        return;
      case "cmd":
        this.pos++;
        this.parseCommand(nodes, t, stop, single);
        return;
      case "space":
        this.pos++;
        return;
    }
  }

  private parseChar(nodes: MathNode[], t: Tok, stop: Stop, single: boolean): void {
    const c = t.v;
    if (/^\p{L}$/u.test(c)) {
      nodes.push({ kind: "ident", text: c });
      return;
    }
    if (/^[0-9]$/.test(c)) {
      nodes.push({ kind: "number", text: c });
      return;
    }
    if (c === ".") {
      const prev = nodes[nodes.length - 1];
      const next = this.peek();
      if (!single && ((prev?.kind === "number" && /[0-9]/.test(prev.text)) || (next?.k === "char" && /^[0-9]$/.test(next.v)))) {
        nodes.push({ kind: "number", text: "." });
        return;
      }
    }
    if (PLAIN_PAIRS[c]) {
      this.parsePlainPair(nodes, t, stop, single);
      return;
    }
    const entry = entryByLatex(c) ?? entryByGlyph(c);
    if (entry) {
      nodes.push(this.symbolNode(entry, c));
      return;
    }
    nodes.push(this.opaque(t.s, t.e, `unknown character ${c}`));
  }

  private symbolNode(entry: CatalogEntry, spelling: string): MathNode {
    if (entry.role === "func") return { kind: "func", name: entry.id };
    if (entry.role === "bigop") return { kind: "bigop", op: entry.id, lower: null, upper: null };
    const node: MathNode = { kind: "symbol", id: entry.id };
    // Keep the source spelling unless it is the canonical (or, in Markdown, the profile's) one.
    const canonical = this.language === "markdown" && entry.latex?.markdown ? entry.latex.markdown : entry.latex?.command;
    if (entry.latex && spelling !== canonical && spelling !== entry.latex.command && spelling.startsWith("\\")) node.command = spelling;
    return node;
  }

  /** `(`...`)` and friends become plain delim nodes when the closing partner exists at this level. */
  private parsePlainPair(nodes: MathNode[], open: Tok, stop: Stop, single: boolean): void {
    const leftOnly = entryByLatex(open.v);
    if (single) {
      nodes.push(leftOnly ? this.symbolNode(leftOnly, open.v) : this.opaque(open.s, open.e, "unknown delimiter"));
      return;
    }
    const closer = PLAIN_PAIRS[open.v] as string;
    const isCloser = (t: Tok) => (t.k === "char" || t.k === "sym" || t.k === "cmd") && t.v === closer;
    const save = this.pos;
    const body = this.parseRow((t) => isCloser(t) || stop(t));
    const t = this.peek();
    const leftEntry = entryByLatex(open.v);
    const rightEntry = entryByLatex(closer);
    if (t && isCloser(t) && leftEntry && rightEntry) {
      this.pos++;
      nodes.push({ kind: "delim", left: leftEntry.id, right: rightEntry.id, body, form: "plain" });
      return;
    }
    // No partner: the opener is an ordinary symbol; re-parse what follows normally.
    this.pos = save;
    if (leftEntry) nodes.push(this.symbolNode(leftEntry, open.v));
    else nodes.push(this.opaque(open.s, open.e, "unknown delimiter"));
  }

  private parseControlSymbol(nodes: MathNode[], t: Tok, stop: Stop, single: boolean): void {
    if (PLAIN_PAIRS[t.v]) {
      this.parsePlainPair(nodes, t, stop, single);
      return;
    }
    if (t.v === "\\\\") {
      nodes.push(this.opaque(t.s, t.e, "line break outside an alignment"));
      return;
    }
    const entry = entryByLatex(t.v);
    if (entry) {
      nodes.push(this.symbolNode(entry, t.v));
      return;
    }
    nodes.push(this.opaque(t.s, t.e, /^\\[,:;!> ]$/.test(t.v) ? "spacing" : `unsupported control symbol ${t.v}`));
  }

  /** Parse a macro argument: a `{...}` group or a single token. Null when missing. */
  private parseArg(): { row: MathRow; end: number } | null {
    this.skipSpace();
    const t = this.peek();
    if (!t || t.k === "close" || t.k === "amp" || this.isRowBreak(t) || this.isEnd(t)) return null;
    if (t.k === "open") {
      this.pos++;
      const r = this.parseRow(() => false);
      const close = this.peek();
      if (close?.k !== "close") return null;
      this.pos++;
      return { row: r, end: close.e };
    }
    if (t.k === "sup" || t.k === "sub") return null;
    const nodes: MathNode[] = [];
    this.singleToken = true;
    this.parseItem(nodes, () => false);
    this.singleToken = false;
    const last = this.toks[this.pos - 1] as Tok;
    return { row: row(nodes), end: last.e };
  }

  /** Raw text of a `{...}` group with balanced braces (no parsing); null when absent. */
  private rawGroup(): { inner: string; start: number; end: number } | null {
    this.skipSpace();
    const t = this.peek();
    if (t?.k !== "open") return null;
    let depth = 0;
    for (let p = this.pos; p < this.toks.length; p++) {
      const x = this.toks[p] as Tok;
      if (x.k === "open") depth++;
      else if (x.k === "close") {
        depth--;
        if (depth === 0) {
          this.pos = p + 1;
          return { inner: this.src.slice(t.e, x.s), start: t.s, end: x.e };
        }
      }
    }
    return null;
  }

  /** Swallow `[...]` and `{...}` arguments right after an unknown command; returns the end offset. */
  private swallowArgs(end: number): number {
    let e = end;
    for (;;) {
      const t = this.peek();
      if (!t) return e;
      if (t.k === "open") {
        const g = this.rawGroup();
        if (!g) return e;
        e = g.end;
        continue;
      }
      if (t.k === "char" && t.v === "[" && t.s === e) {
        const close = this.findChar("]");
        if (close < 0) return e;
        e = (this.toks[close] as Tok).e;
        this.pos = close + 1;
        continue;
      }
      return e;
    }
  }

  private findChar(c: string): number {
    let depth = 0;
    for (let p = this.pos; p < this.toks.length; p++) {
      const x = this.toks[p] as Tok;
      if (x.k === "open") depth++;
      else if (x.k === "close") depth--;
      else if (depth === 0 && x.k === "char" && x.v === c) return p;
    }
    return -1;
  }

  private parseGroupItem(nodes: MathNode[]): void {
    const open = this.peek() as Tok;
    this.pos++;
    const inner = this.parseRow(() => false);
    const close = this.peek();
    if (close?.k !== "close") {
      const last = this.toks[this.toks.length - 1] as Tok;
      this.pos = this.toks.length;
      nodes.push(this.opaque(open.s, last.e, "unbalanced `{`"));
      return;
    }
    this.pos++;
    const next = this.peekNonSpace();
    if (next?.k === "sup" || next?.k === "sub") {
      const node: MathNode = { kind: "scripts", base: inner, sub: null, sup: null };
      this.groupStart.set(node, open.s);
      nodes.push(node);
      return;
    }
    if (inner.children.length === 0) return;
    const only = inner.children.length === 1 ? inner.children[0] : undefined;
    // Groups that change meaning when removed stay verbatim: `{,}` (ordinary comma),
    // `{\rm d}` (scoped switches), anything holding opaque source.
    // Unpaired bracket symbols would pair differently once the braces are gone.
    const keepGroup =
      !isComplete(inner) ||
      (only?.kind === "symbol" && ["bin", "rel", "punct"].includes(classOf(only.id))) ||
      inner.children.some((n) => n.kind === "symbol" && ["open", "close"].includes(classOf(n.id)));
    if (keepGroup) {
      nodes.push(this.opaque(open.s, close.e, "group"));
      return;
    }
    nodes.push(...inner.children);
  }

  private attachScript(nodes: MathNode[], t: Tok, stop: Stop): void {
    const which = t.k === "sup" ? "sup" : "sub";
    const arg = this.parseArg();
    if (!arg) {
      const last = nodes[nodes.length - 1];
      // A `{...}` group waiting for this script becomes part of the verbatim source.
      const groupStart = last ? this.groupStart.get(last) : undefined;
      if (groupStart !== undefined) nodes.pop();
      nodes.push(this.opaqueRest(groupStart ?? t.s, stop, `missing ${which === "sup" ? "superscript" : "subscript"}`));
      return;
    }
    const prev = nodes[nodes.length - 1];
    const bigWhich = which === "sup" ? "upper" : "lower";
    if (prev?.kind === "bigop" && prev[bigWhich] === null) {
      nodes[nodes.length - 1] = { ...prev, [bigWhich]: arg.row };
      return;
    }
    if (prev?.kind === "func" && LIMIT_FUNCS.has(prev.name) && prev.text === undefined) {
      nodes[nodes.length - 1] = { kind: "bigop", op: prev.name, lower: null, upper: null, [bigWhich]: arg.row };
      return;
    }
    if (prev?.kind === "scripts" && prev[which] === null) {
      nodes[nodes.length - 1] = { ...prev, [which]: arg.row };
      return;
    }
    if (prev) {
      nodes[nodes.length - 1] = { kind: "scripts", base: row([prev]), sub: null, sup: null, [which]: arg.row };
      return;
    }
    nodes.push({ kind: "scripts", base: row(), sub: null, sup: null, [which]: arg.row });
  }

  private parseCommand(nodes: MathNode[], t: Tok, stop: Stop, single: boolean): void {
    const name = t.v;
    if (name in FRAC_CMDS) {
      const a = this.parseArg();
      const b = a ? this.parseArg() : null;
      if (!a || !b) {
        nodes.push(this.opaqueRest(t.s, stop, `incomplete ${name}`));
        return;
      }
      const variant = FRAC_CMDS[name];
      nodes.push(variant ? { kind: "frac", num: a.row, den: b.row, variant } : { kind: "frac", num: a.row, den: b.row });
      return;
    }
    if (name === "\\sqrt") {
      this.skipSpace();
      let index: MathRow | null = null;
      const o = this.peek();
      if (o?.k === "char" && o.v === "[") {
        this.pos++;
        index = this.parseRow((x) => x.k === "char" && x.v === "]");
        const c = this.peek();
        if (!(c?.k === "char" && c.v === "]")) {
          nodes.push(this.opaqueRest(t.s, stop, "unterminated root index"));
          return;
        }
        this.pos++;
      }
      const body = this.parseArg();
      if (!body) {
        nodes.push(this.opaqueRest(t.s, stop, "missing radicand"));
        return;
      }
      nodes.push({ kind: "sqrt", body: body.row, index });
      return;
    }
    if (name === "\\left") {
      this.parseLeftRight(nodes, t);
      return;
    }
    if (name === "\\begin") {
      this.parseEnvironment(nodes, t);
      return;
    }
    if (TEXT_CMDS.has(name)) {
      const g = this.rawGroup();
      if (!g || /[\\$^_&#%{}~]/.test(stripTextEscapes(g.inner))) {
        const end = g ? g.end : t.e;
        nodes.push(this.opaque(t.s, end, `unsupported ${name} content`));
        return;
      }
      const text = unescapeText(g.inner);
      nodes.push(name === "\\text" ? { kind: "text", text } : { kind: "text", text, command: name });
      return;
    }
    if (name === "\\operatorname") {
      let end = t.e;
      let star = false;
      const s = this.peek();
      if (s?.k === "char" && s.v === "*") {
        star = true;
        this.pos++;
        end = s.e;
      }
      const g = this.rawGroup();
      if (!g) {
        nodes.push(this.opaque(t.s, end, "\\operatorname needs an argument"));
        return;
      }
      const spelling = `\\operatorname${star ? "*" : ""}{${g.inner}}`;
      const entry = entryByLatex(spelling);
      if (entry?.role === "func") {
        nodes.push({ kind: "func", name: entry.id });
        return;
      }
      if (!star && /^[A-Za-z0-9 -]*$/.test(g.inner)) {
        nodes.push({ kind: "func", name: "func.operatorname", text: g.inner });
        return;
      }
      nodes.push(this.opaque(t.s, g.end, "unsupported \\operatorname"));
      return;
    }
    if (name === "\\not") {
      this.skipSpace();
      const n = this.peek();
      if (n && (n.k === "cmd" || n.k === "char")) {
        const spelling = `\\not${n.v}`;
        const entry = entryByLatex(spelling);
        if (entry) {
          this.pos++;
          nodes.push(this.symbolNode(entry, spelling));
          return;
        }
      }
      nodes.push(this.opaque(t.s, t.e, "\\not needs a relation"));
      return;
    }
    if (name === "\\label" || name === "\\tag" || name === "\\eqref" || name === "\\ref") {
      let end = t.e;
      const s = this.peek();
      if (name === "\\tag" && s?.k === "char" && s.v === "*") {
        this.pos++;
        end = s.e;
      }
      const g = this.rawGroup();
      nodes.push(this.opaque(t.s, g ? g.end : end, name.slice(1)));
      return;
    }
    if (SIZE_CMDS.has(name)) {
      this.skipSpace();
      const d = this.peek();
      if (d && (d.k === "char" || d.k === "sym" || d.k === "cmd")) {
        this.pos++;
        nodes.push(this.opaque(t.s, d.e, "delimiter size"));
        return;
      }
      nodes.push(this.opaque(t.s, t.e, "delimiter size"));
      return;
    }
    if (name === "\\limits" || name === "\\nolimits") {
      const prev = nodes[nodes.length - 1];
      if (prev?.kind === "bigop" && prev.limits === undefined && prev.lower === null && prev.upper === null) {
        nodes[nodes.length - 1] = { ...prev, limits: name === "\\limits" };
        return;
      }
    }
    if (OPAQUE_ONE_TOKEN.has(name)) {
      nodes.push(this.opaque(t.s, t.e, "layout command"));
      return;
    }
    if (PLAIN_PAIRS[name]) {
      this.parsePlainPair(nodes, t, stop, single);
      return;
    }
    const entry = entryByLatex(name);
    if (entry) {
      if (entry.role === "accent" || entry.role === "font") {
        const arg = this.parseArg();
        if (!arg) {
          nodes.push(this.opaqueRest(t.s, stop, `missing argument of ${name}`));
          return;
        }
        // Catalog symbols spelled as a font command (`\mathbb{R}`, `\mathrm{d}`) read back as that symbol.
        const only = arg.row.children.length === 1 ? arg.row.children[0] : undefined;
        if (entry.role === "font" && only?.kind === "ident") {
          const symbol = entryByLatex(`${name}{${only.text}}`);
          if (symbol && symbol.role === "symbol") {
            nodes.push({ kind: "symbol", id: symbol.id });
            return;
          }
        }
        nodes.push(entry.role === "accent" ? { kind: "accent", accent: entry.id, body: arg.row } : { kind: "font", font: entry.id, body: arg.row });
        return;
      }
      nodes.push(this.symbolNode(entry, name));
      return;
    }
    const end = this.swallowArgs(t.e);
    // Catalog symbols written with several tokens (`\\stackrel{\\text{def}}{=}`).
    const spelled = entryByLatex(this.src.slice(t.s, end));
    if (spelled && (spelled.role === "symbol" || spelled.role === "func")) {
      nodes.push(this.symbolNode(spelled, this.src.slice(t.s, end)));
      return;
    }
    nodes.push(this.opaque(t.s, end, `unknown command ${name}`));
  }

  private delimiterToken(): { id: string; end: number } | null {
    this.skipSpace();
    const d = this.peek();
    if (!d) return null;
    if (d.k === "char" && d.v === ".") {
      this.pos++;
      return { id: "delim.none", end: d.e };
    }
    if (d.k === "char" || d.k === "sym" || d.k === "cmd") {
      const entry = entryByLatex(d.v);
      if (entry && (entry.role === "delim" || SIZED_EXTRA.has(entry.id))) {
        this.pos++;
        return { id: entry.id, end: d.e };
      }
    }
    return null;
  }

  private parseLeftRight(nodes: MathNode[], t: Tok): void {
    const save = this.pos;
    const left = this.delimiterToken();
    if (!left) {
      nodes.push(this.opaque(t.s, t.e, "\\left needs a delimiter"));
      return;
    }
    const body = this.parseRow((x) => x.k === "cmd" && x.v === "\\right");
    const r = this.peek();
    if (r?.k === "cmd" && r.v === "\\right") {
      this.pos++;
      const right = this.delimiterToken();
      if (right) {
        nodes.push({ kind: "delim", left: left.id, right: right.id, body });
        return;
      }
    }
    // Unbalanced: keep the whole rest verbatim.
    this.pos = save;
    const end = this.skipToRight();
    nodes.push(this.opaque(t.s, end, "\\left without a matching \\right"));
  }

  /** Skip to the `\\right` matching an unbalanced `\\left`, staying inside the enclosing group. */
  private skipToRight(): number {
    let depth = 0;
    let braces = 0;
    for (let p = this.pos; p < this.toks.length; p++) {
      const x = this.toks[p] as Tok;
      if (x.k === "open") braces++;
      if (x.k === "close" && braces-- === 0) {
        this.pos = p;
        return this.lastEnd(0);
      }
      if (x.k === "cmd" && x.v === "\\left") depth++;
      if (x.k === "cmd" && x.v === "\\right") {
        if (depth === 0) {
          const d = this.toks[p + 1]?.k === "space" ? p + 2 : p + 1;
          const dt = this.toks[d];
          this.pos = dt ? d + 1 : p + 1;
          return dt ? dt.e : x.e;
        }
        depth--;
      }
    }
    this.pos = this.toks.length;
    return this.lastEnd(0);
  }

  private parseEnvironment(nodes: MathNode[], t: Tok): void {
    const g = this.rawGroup();
    if (!g) {
      nodes.push(this.opaque(t.s, t.e, "\\begin needs an environment name"));
      return;
    }
    const env = g.inner.trim();
    const bodyStart = this.pos;
    const isEndOf = (x: Tok) => this.isEnd(x);
    const structured = env in MATRIX_ENVS || env === "cases" || ALIGNED_ENVS.has(env);
    const next = this.peek();
    const hasOptions = next?.k === "char" && (next.v === "[" || next.v === "{") && next.s === g.end;
    if (structured && !hasOptions) {
      this.envDepth++;
      const rows = this.parseCells(isEndOf);
      this.envDepth--;
      const end = this.peek();
      if (end && this.isEnd(end)) {
        const save = this.pos;
        this.pos++;
        const name = this.rawGroup();
        if (name && name.inner.trim() === env) {
          const cleaned = rows;
          if (env in MATRIX_ENVS) {
            nodes.push({ kind: "matrix", fence: MATRIX_ENVS[env] as string, rows: cleaned });
            return;
          }
          if (env === "cases") {
            if (cleaned.every((r) => r.length <= 2)) {
              nodes.push({ kind: "cases", rows: cleaned.map((r): [MathRow, MathRow] => [r[0] ?? row(), r[1] ?? row()]) });
              return;
            }
          } else {
            nodes.push(env === "aligned" ? { kind: "aligned", rows: cleaned } : { kind: "aligned", rows: cleaned, environment: env });
            return;
          }
        }
        this.pos = save;
      }
    }
    // Unknown or malformed environment: keep it verbatim up to its \end.
    this.pos = bodyStart;
    const end = this.skipEnvironment(env);
    nodes.push(this.opaque(t.s, end, `environment ${env}`));
  }

  /** Skip to the `\\end{env}` of an environment, staying inside the enclosing group. */
  private skipEnvironment(env: string): number {
    let depth = 0;
    let braces = 0;
    for (let p = this.pos; p < this.toks.length; p++) {
      const x = this.toks[p] as Tok;
      if (x.k === "open") braces++;
      if (x.k === "close" && braces-- === 0) {
        this.pos = p;
        return this.lastEnd(0);
      }
      if (x.k === "cmd" && (x.v === "\\begin" || x.v === "\\end")) {
        this.pos = p + 1;
        const g = this.rawGroup();
        if (g && g.inner.trim() === env) {
          if (x.v === "\\begin") depth++;
          else if (depth === 0) return g.end;
          else depth--;
        }
        p = this.pos - 1;
      }
    }
    this.pos = this.toks.length;
    return this.lastEnd(0);
  }
}

/** Whether the last line of LaTeX source ends inside a `%` comment. */
function endsInComment(source: string): boolean {
  if (source.endsWith("\n")) return false;
  const line = source.slice(source.lastIndexOf("\n") + 1);
  for (let i = 0; i < line.length; i++) {
    if (line[i] === "\\") i++;
    else if (line[i] === "%") return true;
  }
  return false;
}

/** Reason of the opaque node holding a row break's `*` / `[len]` (first node of the next row). */
export const ROW_BREAK_MODIFIER = "row break modifier";

/** Separate a row's leading row-break modifier from its cells. */
function splitModifier(cells: MathRow[]): { mod: string; cells: MathRow[] } {
  const first = cells[0]?.children[0];
  if (first?.kind !== "opaque" || first.reason !== ROW_BREAK_MODIFIER) return { mod: "", cells };
  return { mod: first.source, cells: [row((cells[0] as MathRow).children.slice(1)), ...cells.slice(1)] };
}

/** An empty last row is written as `{}` so that it is not read back as a trailing `\\\\`. */
function keepLastRow(rows: string[]): string[] {
  return rows.length > 1 && rows[rows.length - 1] === "" ? [...rows.slice(0, -1), "{}"] : rows;
}

function dropTrailingEmptyRow(rows: MathRow[][]): MathRow[][] {
  const last = rows[rows.length - 1];
  if (rows.length > 1 && last && last.length === 1 && isEmptyRow(last[0])) return rows.slice(0, -1);
  return rows;
}

export function parseLatexMath(content: string, language: MathLanguage = "latex"): ParseResult {
  const parser = new LatexParser(content, language);
  const r = parser.parseContent();
  return { row: r, complete: isComplete(r) };
}

// ================================================================ serializer

export type LatexDialect = "latex" | "markdown" | "preview";

type Cls = "ord" | "bin" | "rel" | "op" | "open" | "close" | "punct";
interface Piece {
  text: string;
  cls: Cls;
  /** Verbatim source (an unknown macro must not take a following group as its argument). */
  opaque?: boolean;
}

const MARKDOWN_TEXT_CMDS = new Set(["\\text", "\\textrm", "\\textnormal", "\\mbox", "\\textit", "\\textbf"]);
const MARKDOWN_DELIMS = new Set([
  "delim.paren.l",
  "delim.paren.r",
  "delim.bracket.l",
  "delim.bracket.r",
  "delim.brace.l",
  "delim.brace.r",
  "delim.angle.l",
  "delim.angle.r",
  "delim.floor.l",
  "delim.floor.r",
  "delim.ceil.l",
  "delim.ceil.r",
  "delim.vert",
  "delim.vert.l",
  "delim.vert.r",
  "delim.vert.cmd",
  "delim.Vert",
  "delim.Vert.cmd",
  "delim.Vert.l",
  "delim.Vert.r",
  "delim.none",
  "op.slash",
  "arrow.t",
  "arrow.b",
]);
/** Opaque LaTeX the Markdown translator accepts (spacing, styles, numbering, sizes). */
const MARKDOWN_OPAQUE_OK = /^(\\[,:;! >]|\\quad|\\qquad|~|\\nonumber|\\notag|\\displaystyle|\\textstyle|\\limits|\\nolimits|\\(big|Big|bigg|Bigg)[lrm]?\s*\S.*)$/s;
/** Opaque LaTeX that KaTeX renders, passed through in previews. */
const PREVIEW_OPAQUE_OK =
  /^(\\[,:;! >]|\\quad|\\qquad|~|\\displaystyle|\\textstyle|\\scriptstyle|\\limits|\\nolimits|\\(big|Big|bigg|Bigg)[lrm]?\s*\S.*|\\middle\s*\S.*|\{.*\}|\\begin\{(array|smallmatrix|alignedat|gathered|split|subarray|CD|darray|dcases|rcases)\}.*)$/s;
/** Typst spacing names (opaque in Typst) and their LaTeX commands. */
const TYPST_SPACING: Record<string, string> = { thin: "\\,", med: "\\:", thick: "\\;", quad: "\\quad", wide: "\\qquad" };
const PREVIEW_DROP = /^(\\label|\\tag|\\nonumber|\\notag|%)/;

class LatexWriter {
  readonly packages: string[] = [];
  readonly unsupported: { what: string; reason: string }[] = [];
  constructor(readonly dialect: LatexDialect) {}

  private need(pkg: string | undefined): void {
    if (pkg && this.dialect === "latex" && !this.packages.includes(pkg)) this.packages.push(pkg);
  }

  private reject(what: string, reason: string): void {
    if (this.dialect === "preview") return;
    // One report per catalog id or kind; opaque source is reported per fragment.
    if (!this.unsupported.some((u) => u.what === what && (what !== "opaque" || u.reason === reason))) this.unsupported.push({ what, reason });
  }

  private entry(id: string): CatalogEntry | undefined {
    const e = entryById(id);
    if (!e) this.reject(id, "unknown catalog id");
    return e;
  }

  /** Spelling for an entry (honoring a preserved alternative); checks Markdown support. */
  private spell(id: string, preferred?: string): string {
    const e = this.entry(id);
    if (!e || !e.latex) {
      if (e) this.reject(id, "no LaTeX command");
      return "";
    }
    const spellings = [e.latex.command, ...(e.latex.alternatives ?? [])];
    let s = preferred && spellings.includes(preferred) ? preferred : e.latex.command;
    if (this.dialect === "markdown") {
      if (!e.markdown) this.reject(id, `the Markdown print profile does not support ${e.latex.command}`);
      else if (s === e.latex.command && e.latex.markdown) s = e.latex.markdown;
    }
    if (s === e.latex.command || !s.startsWith("\\not")) this.need(e.latex.package);
    return s;
  }

  row(r: MathRow, script = false, slot = false): string {
    if (r.children.length === 0) return slot && this.dialect === "preview" ? "\\textcolor{gray}{\\square}" : "";
    const pieces = r.children.map((n) => this.node(n)).filter((p) => p.text !== "");
    return joinPieces(pieces, script);
  }

  private group(r: MathRow, script = false): string {
    return `{${this.row(r, script, true)}}`;
  }

  /** Script argument: a single ASCII letter or digit goes bare, anything else in braces. */
  private scriptArg(r: MathRow): string {
    const only = r.children.length === 1 ? r.children[0] : undefined;
    if (only && (only.kind === "ident" || only.kind === "number") && /^[A-Za-z0-9]$/.test(only.text)) return only.text;
    return this.group(r, true);
  }

  private scripts(sub: MathRow | null, sup: MathRow | null): string {
    let s = "";
    if (sub) s += `_${this.scriptArg(sub)}`;
    if (sup) s += `^${this.scriptArg(sup)}`;
    return s;
  }

  private delimText(id: string): string {
    if (id === "delim.none") return ".";
    if (this.dialect === "markdown" && !MARKDOWN_DELIMS.has(id)) this.reject(id, "the Markdown print profile cannot size this delimiter");
    return this.spell(id);
  }

  node(n: MathNode): Piece {
    switch (n.kind) {
      case "ident":
      case "number":
        return { text: n.text, cls: "ord" };
      case "symbol": {
        const text = this.spell(n.id, n.command);
        return { text, cls: classOf(n.id) };
      }
      case "text": {
        const cmd = n.command ?? "\\text";
        if (cmd === "\\text") this.need("amsmath");
        if (this.dialect === "markdown" && !MARKDOWN_TEXT_CMDS.has(cmd)) this.reject(cmd, "the Markdown print profile does not support it");
        return { text: `${cmd}{${escapeText(n.text)}}`, cls: "ord" };
      }
      case "frac": {
        const variant = n.variant === "slash" ? undefined : n.variant;
        const cmd = variant === "binom" ? "\\binom" : variant ? `\\${variant}` : "\\frac";
        if (variant) this.need("amsmath");
        return { text: `${cmd}${this.group(n.num)}${this.group(n.den)}`, cls: "ord" };
      }
      case "sqrt":
        return { text: `\\sqrt${n.index ? `[${this.row(n.index, true, true)}]` : ""}${this.group(n.body)}`, cls: "ord" };
      case "scripts": {
        const only = n.base.children.length === 1 ? n.base.children[0] : undefined;
        let base: string;
        // Opaque source keeps its braces (unless it is a bare macro such as `\\R`).
        const bareMacro = only?.kind === "opaque" && /^\\[A-Za-z]+$/.test(only.source) && only.reason.startsWith("unknown command");
        if (!only || only.kind === "scripts" || only.kind === "bigop" || (only.kind === "opaque" && !bareMacro)) base = this.group(n.base);
        else base = this.node(only).text;
        if (only?.kind === "symbol" && only.id === "calc.prime") base = this.node(only).text;
        return { text: `${base}${this.scripts(n.sub, n.sup)}`, cls: "ord" };
      }
      case "bigop": {
        const e = this.entry(n.op);
        if (e?.role === "func") return { text: `${this.spell(n.op)}${this.scripts(n.lower, n.upper)}`, cls: "op" };
        const limits = n.limits === undefined ? "" : n.limits ? "\\limits" : "\\nolimits";
        return { text: `${this.spell(n.op)}${limits}${this.scripts(n.lower, n.upper)}`, cls: "op" };
      }
      case "func": {
        if (n.name === "func.operatorname") {
          this.need("amsmath");
          return { text: `\\operatorname{${n.text ?? ""}}`, cls: "op" };
        }
        return { text: this.spell(n.name), cls: classOf(n.name) };
      }
      case "delim": {
        const body = this.row(n.body, false, true);
        if (n.form === "plain") {
          const l = n.left === "delim.none" ? "" : this.spell(n.left);
          const r = n.right === "delim.none" ? "" : this.spell(n.right);
          return { text: `${l}${spaceAfterWord(l, body)}${body}${spaceAfterWord(body, r)}${r}`, cls: "ord" };
        }
        const l = this.delimText(n.left);
        const r = this.delimText(n.right);
        return { text: `\\left${l}${body ? " " : ""}${body}${body ? " " : ""}\\right${r}`, cls: "ord" };
      }
      case "accent": {
        const cmd = this.spell(n.accent);
        return { text: `${cmd}${this.group(n.body)}`, cls: "ord" };
      }
      case "font": {
        const cmd = this.spell(n.font);
        return { text: `${cmd}${this.group(n.body)}`, cls: "ord" };
      }
      case "matrix": {
        const env = FENCE_ENV[n.fence] ?? "matrix";
        if (!FENCE_ENV[n.fence]) this.reject(n.fence, "unknown matrix fence");
        this.need("amsmath");
        return { text: `\\begin{${env}} ${this.grid(n.rows, " \\\\ ")} \\end{${env}}`, cls: "ord" };
      }
      case "cases": {
        this.need("amsmath");
        const parts = n.rows.map(([v, c]) => {
          const { mod, cells } = splitModifier([v, c]);
          const [value, condition] = cells as [MathRow, MathRow];
          return { mod, text: isEmptyRow(condition) ? this.row(value, false, true) : `${this.row(value, false, true)} & ${this.row(condition, false, true)}` };
        });
        const body = keepLastRow(parts.map((x) => x.text)).reduce((out, text, i) => (i === 0 ? text : out + this.rowBreak(" \\\\ ", (parts[i] as { mod: string }).mod) + text), "");
        return { text: `\\begin{cases} ${body} \\end{cases}`, cls: "ord" };
      }
      case "aligned": {
        if (n.bare) return { text: this.grid(n.rows, " \\\\\n"), cls: "ord" };
        const env = n.environment ?? "aligned";
        this.need("amsmath");
        if (this.dialect === "markdown" && !["aligned", "gathered", "split"].includes(env)) this.reject(env, "unsupported environment");
        return { text: `\\begin{${env}} ${this.grid(n.rows, " \\\\ ")} \\end{${env}}`, cls: "ord" };
      }
      case "opaque":
        return { text: this.opaque(n), cls: "ord", opaque: true };
    }
  }

  /** Cells joined by `&`; a cell starting with a relation attaches to it (`a &= b`). */
  private grid(rows: MathRow[][], rowSep: string): string {
    const parts = rows.map((cells) => splitModifier(cells));
    const texts = parts.map(({ cells }) =>
        cells
          .map((c, i) => {
            const text = this.row(c, false, true);
            if (i === 0) return text;
            const first = c.children[0];
            const tight = !text || (first?.kind === "symbol" && classOf(first.id) === "rel");
            return `${tight ? "&" : "& "}${text}`;
          })
          .join(" ")
          .replace(/^ /, ""),
      );
    return keepLastRow(texts).reduce((out, text, i) => (i === 0 ? text : out + this.rowBreak(rowSep, (parts[i] as { mod: string }).mod) + text), "");
  }

  /** The row separator with a preserved `*` / `[len]` modifier attached to `\\\\`. */
  private rowBreak(sep: string, mod: string): string {
    if (!mod) return sep;
    if (this.dialect === "markdown") this.reject("opaque", `the Markdown print profile does not support \\\\${mod}`);
    const kept = this.dialect === "preview" ? mod.replace(/^\*/, "") : mod;
    return sep.replace("\\\\", () => `\\\\${kept}`);
  }

  private opaque(n: Extract<MathNode, { kind: "opaque" }>): string {
    if (n.language === "typst") {
      const spacing = TYPST_SPACING[n.source];
      if (spacing) return spacing;
      if (this.dialect === "preview") return `\\textcolor{gray}{\\text{${escapeText(n.source)}}}`;
      this.reject("opaque", `Typst source cannot be written as LaTeX: ${n.source}`);
      return "";
    }
    if (this.dialect === "preview") {
      if (PREVIEW_DROP.test(n.source)) return "";
      if (PREVIEW_OPAQUE_OK.test(n.source)) return n.source;
      return `\\textcolor{gray}{\\text{${escapeText(n.source)}}}`;
    }
    if (this.dialect === "markdown" && !MARKDOWN_OPAQUE_OK.test(n.source)) {
      this.reject("opaque", `the Markdown print profile does not support ${n.source}`);
    }
    // A comment must end its line.
    // Source ending inside a `%` comment must end its line.
    return n.language === "latex" && endsInComment(n.source) ? `${n.source}\n` : n.source;
  }
}

function spaceAfterWord(prev: string, next: string): string {
  return /\\[A-Za-z]+$/.test(prev) && /^[A-Za-z0-9]/.test(next) ? " " : "";
}

function joinPieces(pieces: Piece[], script: boolean): string {
  // Unary binary operators (at the start or after another operator) are ordinary.
  const cls: Cls[] = pieces.map((p, i) => {
    if (p.cls !== "bin") return p.cls;
    const prev = i > 0 ? (pieces[i - 1] as Piece).cls : undefined;
    return prev === undefined || ["bin", "rel", "open", "punct", "op"].includes(prev) ? "ord" : "bin";
  });
  let out = "";
  pieces.forEach((p, i) => {
    if (i > 0) {
      const prevText = (pieces[i - 1] as Piece).text;
      const a = cls[i - 1] as Cls;
      const b = cls[i] as Cls;
      let sep = "";
      if (!script && (a === "bin" || b === "bin" || a === "rel" || b === "rel" || a === "punct")) sep = " ";
      if (prevText.startsWith("%")) sep = "";
      else if ((pieces[i - 1] as Piece).opaque && /[A-Za-z\]}]$/.test(prevText) && /^[{[]/.test(p.text)) sep = " ";
      else if (sep === "" && /\\[A-Za-z]+$/.test(prevText) && /^[A-Za-z0-9]/.test(p.text)) sep = " ";
      else if (sep === "" && !script && /^[A-Za-z0-9\\]/.test(p.text)) {
        // Readability: after a group, a bare script, or an operator; before \text.
        if ((/\}$/.test(prevText) && /^[A-Za-z\\]/.test(p.text)) || /[_^]([0-9]+|[A-Za-z])$/.test(prevText) || a === "op" || (/^\\text/.test(p.text) && /[A-Za-z0-9]$/.test(prevText))) sep = " ";
      }
      out += sep;
    }
    out += p.text;
  });
  return out;
}

/** Escapes `escapeText` writes (and LaTeX's other text escapes), removed for validation. */
const TEXT_ESCAPE = /\\textbackslash(\{\}|(?![A-Za-z]))|\\textasciitilde(\{\}|(?![A-Za-z]))|\\textasciicircum(\{\}|(?![A-Za-z]))|\\\^\{\}|\\~\{\}|\\[{}$&#%_ ]/g;

function stripTextEscapes(s: string): string {
  return s.replace(TEXT_ESCAPE, "");
}

function unescapeText(s: string): string {
  return s.replace(TEXT_ESCAPE, (m) => {
    if (m.startsWith("\\textbackslash")) return "\\";
    if (m.startsWith("\\textasciitilde") || m === "\\~{}") return "~";
    if (m.startsWith("\\textasciicircum") || m === "\\^{}") return "^";
    return m.slice(1);
  });
}

function escapeText(s: string): string {
  return s.replace(/[\\{}$&#^_%~]/g, (c) =>
    c === "\\" ? "\\textbackslash{}" : c === "^" ? "\\^{}" : c === "~" ? "\\~{}" : `\\${c}`,
  );
}

export function serializeLatexMath(r: MathRow, dialect: LatexDialect = "latex"): SerializeResult {
  const w = new LatexWriter(dialect);
  const source = w.row(r);
  return {
    source,
    requirements: w.packages.map((name): Requirement => ({ kind: "latexPackage", name })),
    unsupported: w.unsupported,
  };
}

// ================================================================ requirements

/** Packages that provide (or replace) another package's commands. */
const PROVIDES: Record<string, string[]> = {
  mathtools: ["amsmath"],
  amssymb: ["amsfonts"],
  "unicode-math": ["amssymb", "amsfonts", "mathrsfs"],
  esint: [],
};

function usedPackages(text: string): string[] {
  const out: string[] = [];
  const re = /\\(?:usepackage|RequirePackage)\s*(?:\[[^\]]*\])?\s*\{([^}]*)\}/g;
  const stripped = stripComments(text);
  for (const m of stripped.matchAll(re)) {
    for (const name of (m[1] ?? "").split(",")) {
      const n = name.trim();
      if (n) out.push(n);
    }
  }
  return out;
}

/** Replace comments with spaces (offsets preserved). */
export function stripComments(text: string): string {
  let out = "";
  let i = 0;
  while (i < text.length) {
    const c = text[i] as string;
    if (c === "\\") {
      out += text.slice(i, i + 2);
      i += 2;
      continue;
    }
    if (c === "%") {
      let j = text.indexOf("\n", i);
      if (j < 0) j = text.length;
      out += " ".repeat(j - i);
      i = j;
      continue;
    }
    out += c;
    i++;
  }
  return out;
}

export function latexSatisfied(text: string, req: Requirement): boolean {
  const used = usedPackages(text);
  return used.includes(req.name) || used.some((p) => (PROVIDES[p] ?? []).includes(req.name));
}

export function latexAddRequirement(text: string, req: Requirement): { offset: number; insert: string } | null {
  const stripped = stripComments(text);
  const begin = findDocumentMarker(text, "begin");
  const limit = begin ? begin.start : stripped.length;
  const nl = text.includes("\r\n") ? "\r\n" : "\n";
  let anchor = -1;
  for (const m of stripped.slice(0, limit).matchAll(/\\(?:usepackage|documentclass)\s*(?:\[[^\]]*\])?\s*\{[^}]*\}/g)) {
    anchor = (m.index ?? 0) + m[0].length;
  }
  const line = `\\usepackage{${req.name}}`;
  if (anchor < 0) {
    // No package line yet: directly before \begin{document}, when there is one.
    if (!begin) return null;
    return { offset: begin.start, insert: `${begin.start > 0 && text[begin.start - 1] !== "\n" ? nl : ""}${line}${nl}` };
  }
  const lineEnd = text.indexOf("\n", anchor);
  // Always in the preamble: never after \begin{document}, even when it shares the line.
  if (begin && (lineEnd < 0 || lineEnd + 1 > begin.start)) return { offset: begin.start, insert: `${nl}${line}${nl}` };
  if (lineEnd < 0) return { offset: text.length, insert: `${nl}${line}` };
  return { offset: lineEnd + 1, insert: `${line}${nl}` };
}

// ================================================================ adapter

export const latexAdapter: MathLanguageAdapter = {
  language: "latex",
  equations: (text) => scanLatex(text).equations,
  contextAt(text, offset) {
    const scan = scanLatex(text);
    return contextFrom(scan.equations, scan.unavailable, offset);
  },
  parse: (content) => parseLatexMath(content, "latex"),
  serialize: (r) => serializeLatexMath(r, "latex"),
  wrap: (content, display) => (display ? `\\[\n  ${content}\n\\]` : `$${content}$`),
  satisfied: latexSatisfied,
  addRequirement: latexAddRequirement,
};
