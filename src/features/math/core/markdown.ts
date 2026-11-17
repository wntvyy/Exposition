// Markdown print profile: `$...$` / `$$...$$` under pulldown-cmark's math
// rules (crates/exposition-markdown uses pulldown-cmark 0.13 with
// ENABLE_MATH); the content is the LaTeX subset translated by
// crates/exposition-markdown/src/math.rs.
import { contextFrom, parseLatexMath, serializeLatexMath, type Region } from "./latex";
import type { EquationRange, MathLanguageAdapter } from "./types";

const ASCII_PUNCT = /[!-/:-@[-`{-~]/;
const isAsciiSpace = (c: string | undefined) => c === " " || c === "\t" || c === "\n" || c === "\r" || c === "\f" || c === "\v";

interface Block {
  start: number;
  end: number;
}

interface MarkdownScan {
  equations: EquationRange[];
  unavailable: Region[];
}

interface Line {
  start: number;
  /** End of the line content (before the line break). */
  end: number;
  /** Start of the next line. */
  next: number;
}

function splitLines(text: string): Line[] {
  const lines: Line[] = [];
  let i = 0;
  while (i <= text.length) {
    const nl = text.indexOf("\n", i);
    const end = nl < 0 ? text.length : nl;
    const contentEnd = end > i && text[end - 1] === "\r" ? end - 1 : end;
    lines.push({ start: i, end: contentEnd, next: nl < 0 ? text.length + 1 : nl + 1 });
    if (nl < 0) break;
    i = nl + 1;
  }
  return lines;
}

const LIST_MARKER = /^(\s{0,3})([-+*]|\d{1,9}[.)])(\s+|$)/;

/** Block structure: paragraphs/headings/table rows (inline content), code, front matter, HTML comments. */
function blocks(text: string): { inline: Block[]; unavailable: Region[] } {
  const inline: Block[] = [];
  const unavailable: Region[] = [];
  const lines = splitLines(text);
  let li = 0;
  // Front matter.
  if (/^---\s*$/.test(text.slice(0, lines[0]?.end ?? 0)) && lines.length > 1) {
    for (let j = 1; j < lines.length; j++) {
      const l = lines[j] as Line;
      if (/^(---|\.\.\.)\s*$/.test(text.slice(l.start, l.end))) {
        unavailable.push({ start: 0, end: l.end, reason: "front matter", startIn: true, endIn: true });
        li = j + 1;
        break;
      }
    }
  }
  let para: Block | null = null;
  let listContext = false;
  const flush = () => {
    if (para) inline.push(para);
    para = null;
  };
  for (; li < lines.length; li++) {
    const l = lines[li] as Line;
    const raw = text.slice(l.start, l.end);
    // Container prefixes (block quotes) are part of the block; strip them for detection.
    const quote = /^(\s{0,3}>\s?)*/.exec(raw)?.[0] ?? "";
    const body = raw.slice(quote.length);
    const bodyStart = l.start + quote.length;
    if (body.trim() === "") {
      flush();
      continue;
    }
    const indent = /^[ \t]*/.exec(body)?.[0] ?? "";
    const width = [...indent].reduce((w, c) => w + (c === "\t" ? 4 - (w % 4) : 1), 0);
    const fence = /^[ \t]*(`{3,}|~{3,})/.exec(body);
    if (fence && (width < 4 || listContext) && !(fence[1]?.startsWith("`") && body.slice(fence[0].length).includes("`"))) {
      flush();
      const marker = fence[1] as string;
      let end = text.length;
      let j = li + 1;
      for (; j < lines.length; j++) {
        const m = lines[j] as Line;
        const t = text.slice(m.start, m.end).replace(/^(\s{0,3}>\s?)*/, "");
        const close = /^[ \t]*(`{3,}|~{3,})[ \t]*$/.exec(t);
        if (close && close[1]?.[0] === marker[0] && (close[1]?.length ?? 0) >= marker.length) {
          end = m.end;
          break;
        }
      }
      unavailable.push({ start: l.start, end, reason: "code block", endIn: true });
      li = j;
      continue;
    }
    if (width >= 4 && !para && !listContext) {
      unavailable.push({ start: l.start, end: l.end, reason: "code block", endIn: true });
      continue;
    }
    if (/^\s{0,3}<!--/.test(body)) {
      flush();
      const close = text.indexOf("-->", bodyStart);
      const end = close < 0 ? text.length : close + 3;
      unavailable.push({ start: bodyStart, end, reason: "HTML comment" });
      while (li + 1 < lines.length && (lines[li + 1] as Line).start < end) li++;
      continue;
    }
    if (width < 4) {
      if (/^\s{0,3}#{1,6}(\s|$)/.test(body) || /^\s{0,3}\|/.test(body)) {
        flush();
        inline.push({ start: bodyStart, end: l.end });
        listContext = false;
        continue;
      }
      if (/^\s{0,3}([-*_])(\s*\1){2,}\s*$/.test(body) || /^\s{0,3}=+\s*$/.test(body)) {
        flush();
        continue;
      }
      const list = LIST_MARKER.exec(body);
      if (list) {
        flush();
        listContext = true;
        para = { start: bodyStart + list[0].length, end: l.end };
        continue;
      }
      if (width === 0 && !para) listContext = false;
    }
    if (para) para.end = l.end;
    else para = { start: bodyStart, end: l.end };
  }
  flush();
  return { inline, unavailable };
}

interface Dollar {
  pos: number;
  canOpen: boolean;
  canClose: boolean;
  ctx: number;
}

/** Inline math and code spans inside one block (pulldown-cmark semantics). */
function scanInline(text: string, b: Block, equations: EquationRange[], unavailable: Region[]): void {
  // Pass 1: dollar items with brace contexts (firstpass.rs).
  const items: Dollar[] = [];
  const stack: number[] = [];
  let nextCtx = 1;
  for (let i = b.start; i < b.end; i++) {
    const c = text[i];
    if (c === "\\" && ASCII_PUNCT.test(text[i + 1] ?? "") && i + 1 < b.end) {
      i++;
      continue;
    }
    if (c === "$") {
      if (stack.length === 0) stack.push(0);
      items.push({
        pos: i,
        canOpen: i + 1 < b.end && !isAsciiSpace(text[i + 1]),
        canClose: i > b.start && !isAsciiSpace(text[i - 1]),
        ctx: stack[stack.length - 1] as number,
      });
    } else if (c === "{") {
      if (stack.length > 0) stack.push(nextCtx++);
    } else if (c === "}") {
      if (stack.length === 1) stack[0] = nextCtx++;
      else stack.pop();
    }
  }
  const index = new Map(items.map((d, k) => [d.pos, k]));
  const adjacent = (k: number) => (items[k + 1]?.pos ?? -1) === (items[k] as Dollar).pos + 1;
  // Pass 2: left to right, code spans and math spans.
  let i = b.start;
  while (i < b.end) {
    const c = text[i];
    if (c === "\\" && ASCII_PUNCT.test(text[i + 1] ?? "")) {
      i += 2;
      continue;
    }
    if (c === "`") {
      let n = 0;
      while (text[i + n] === "`") n++;
      const fence = "`".repeat(n);
      let j = text.indexOf(fence, i + n);
      while (j >= 0 && j < b.end && (text[j + n] === "`" || text[j - 1] === "`")) {
        let k = j;
        while (text[k] === "`") k++;
        j = text.indexOf(fence, k);
      }
      if (j >= 0 && j + n <= b.end) {
        unavailable.push({ start: i, end: j + n, reason: "code" });
        i = j + n;
      } else i += n;
      continue;
    }
    if (c !== "$") {
      i++;
      continue;
    }
    const k = index.get(i);
    const open = k === undefined ? undefined : items[k];
    if (k === undefined || !open || !open.canOpen) {
      i++;
      continue;
    }
    const isDisplay = adjacent(k);
    let found = -1;
    for (let j = isDisplay ? k + 2 : k + 1; j < items.length; j++) {
      const d = items[j] as Dollar;
      if (d.ctx !== open.ctx) continue;
      if ((!isDisplay && d.canClose) || (isDisplay && adjacent(j))) found = j;
      break;
    }
    if (found < 0) {
      i++;
      continue;
    }
    const close = items[found] as Dollar;
    const startDisplay = isDisplay && found !== k + 1;
    const display = startDisplay && adjacent(found);
    if (display) {
      const end = (items[found + 1] as Dollar).pos + 1;
      equations.push({ start: open.pos, end, contentStart: open.pos + 2, contentEnd: close.pos, display: true, style: "$$" });
      i = end;
      continue;
    }
    if (close.pos === open.pos + 1) {
      i++;
      continue;
    }
    equations.push({ start: open.pos, end: close.pos + 1, contentStart: open.pos + 1, contentEnd: close.pos, display: false, style: "$" });
    i = close.pos + 1;
  }
}

export function scanMarkdown(text: string): MarkdownScan {
  const { inline, unavailable } = blocks(text);
  const equations: EquationRange[] = [];
  for (const b of inline) scanInline(text, b, equations, unavailable);
  equations.sort((a, b) => a.start - b.start);
  return { equations, unavailable };
}

export const markdownAdapter: MathLanguageAdapter = {
  language: "markdown",
  equations: (text) => scanMarkdown(text).equations,
  contextAt(text, offset) {
    const scan = scanMarkdown(text);
    return contextFrom(scan.equations, scan.unavailable, offset);
  },
  parse: (content) => parseLatexMath(content, "markdown"),
  serialize: (r) => serializeLatexMath(r, "markdown"),
  // Inline math may not start or end with whitespace.
  wrap: (content, display) => (display ? `$$\n${content}\n$$` : `$${content.trim()}$`),
  satisfied: () => true,
  addRequirement: () => null,
};
