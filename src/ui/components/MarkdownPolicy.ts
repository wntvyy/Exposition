// Pure policy helpers of the shared Markdown renderer (Markdown.tsx): which
// addresses may be offered as links, how math delimiters are normalized, which
// fence languages are highlighted or runnable, and whether a block is still
// open while a response streams. No React and no DOM here, so the security
// rules are unit-testable on their own.

/** Longest address `app_open_external` accepts (src-tauri/src/app.rs). */
export const MAX_EXTERNAL_URL = 2048;

/** Invisible and direction-changing characters: the shown address would differ from the one opened. */
const INVISIBLE = /[­͏؜᠎​-‏‪-‮⁠-⁯﻿￹-￻]/u;
// eslint-disable-next-line no-control-regex
const SPACE_OR_CONTROL = /[\s\u0000-\u001F\u007F-\u009F]/u;

/**
 * The address a link or image may open in the system browser, or null when it
 * must stay inert. Mirrors `check_external_url` in src-tauri/src/app.rs (which
 * re-checks it): absolute http/https with a host, no embedded credentials, no
 * whitespace, control, invisible, or bidi characters, at most 2048 characters.
 * `javascript:`, `data:`, `file:`, `vbscript:`, `tauri:`, `ipc:`, `mailto:`,
 * relative and fragment links all return null.
 */
export function externalUrl(raw: string): string | null {
  if (raw.length === 0 || raw.length > MAX_EXTERNAL_URL) return null;
  if (SPACE_OR_CONTROL.test(raw) || INVISIBLE.test(raw)) return null;
  // Only an explicit scheme counts: "//host/x" or "www.x" without a scheme stay inert
  // (remark-gfm already turns bare www. autolinks into http:// addresses).
  if (!/^https?:\/\//i.test(raw)) return null;
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  if (!url.hostname) return null;
  if (url.username || url.password) return null;
  return raw;
}

/** An address shown as inert text: long `data:` payloads are shortened for display only. */
export function displayAddress(raw: string, max = 160): string {
  return raw.length > max ? `${raw.slice(0, max)}…` : raw;
}

// ---------------------------------------------------------------- math

/**
 * Normalize the LaTeX delimiters models use, `\(…\)` and `\[…\]`, to the
 * `$…$` / `$$…$$` forms remark-math parses. Code (fenced, indented by a fence,
 * and inline spans) is left untouched, as is an escaped backslash (`\\(`).
 * The original text is kept by the caller for copying; this is render-only.
 */
export function normalizeMathDelimiters(source: string): string {
  if (!source.includes("\\(") && !source.includes("\\[")) return source;
  const lines = source.split("\n");
  const out: string[] = [];
  let fence: { char: string; size: number } | null = null;
  let displayOpen = false;
  for (const line of lines) {
    const fenceMatch = /^ {0,3}(`{3,}|~{3,})/.exec(line);
    if (fence) {
      out.push(line);
      if (fenceMatch && fenceMatch[1]![0] === fence.char && fenceMatch[1]!.length >= fence.size && line.trim() === fenceMatch[1]) {
        fence = null;
      }
      continue;
    }
    if (fenceMatch && !displayOpen) {
      fence = { char: fenceMatch[1]![0]!, size: fenceMatch[1]!.length };
      out.push(line);
      continue;
    }
    const converted = convertLine(line, displayOpen);
    displayOpen = converted.displayOpen;
    out.push(converted.text);
  }
  return out.join("\n");
}

/** Convert delimiters on one line outside inline code spans. */
function convertLine(line: string, displayOpenAtStart: boolean): { text: string; displayOpen: boolean } {
  let displayOpen = displayOpenAtStart;
  let result = "";
  let i = 0;
  while (i < line.length) {
    const ch = line[i]!;
    if (ch === "`") {
      // An inline code span: copy it verbatim up to the matching run of backticks.
      let run = 1;
      while (line[i + run] === "`") run += 1;
      const ticks = "`".repeat(run);
      const close = line.indexOf(ticks, i + run);
      if (close >= 0 && line[close + run] !== "`") {
        result += line.slice(i, close + run);
        i = close + run;
        continue;
      }
      result += ticks;
      i += run;
      continue;
    }
    if (ch === "\\") {
      const next = line[i + 1];
      if (next === "\\") {
        // An escaped backslash is literal text.
        result += "\\\\";
        i += 2;
        continue;
      }
      if (next === "[" && !displayOpen) {
        displayOpen = true;
        result += "\n$$\n";
        i += 2;
        continue;
      }
      if (next === "]" && displayOpen) {
        displayOpen = false;
        result += "\n$$\n";
        i += 2;
        continue;
      }
      if (next === "(" && !displayOpen) {
        const close = findInlineClose(line, i + 2);
        if (close >= 0) {
          result += `$${line.slice(i + 2, close)}$`;
          i = close + 2;
          continue;
        }
      }
    }
    result += ch;
    i += 1;
  }
  return { text: result, displayOpen };
}

function findInlineClose(line: string, from: number): number {
  for (let j = from; j < line.length - 1; j += 1) {
    if (line[j] === "\\") {
      if (line[j + 1] === ")") return j;
      j += 1; // skip the escaped character
    }
  }
  return -1;
}

// ---------------------------------------------------------------- streaming

/**
 * While a response streams, whether the block whose source ends at
 * `endOffset` is still open (its closing fence has not arrived). An open
 * block reaches the end of the text and its last line is not a closing fence.
 */
export function isOpenFence(source: string, startOffset: number, endOffset: number): boolean {
  if (endOffset < source.trimEnd().length) return false;
  const block = source.slice(startOffset, endOffset);
  const opening = /^ {0,3}(`{3,}|~{3,})/.exec(block);
  if (!opening) return false;
  const lines = block.trimEnd().split("\n");
  if (lines.length < 2) return true;
  const last = lines[lines.length - 1]!.trim();
  const char = opening[1]![0]!;
  return !(last.length >= opening[1]!.length && [...last].every((c) => c === char));
}

/** Like `isOpenFence` for a `$$` display-math block. */
export function isOpenMath(source: string, startOffset: number, endOffset: number): boolean {
  if (endOffset < source.trimEnd().length) return false;
  const block = source.slice(startOffset, endOffset).trim();
  if (!block.startsWith("$$")) return false;
  return block.length < 4 || !block.endsWith("$$");
}

// ---------------------------------------------------------------- languages

/** Runtime languages a fence can be run in through the user's research session. */
export type RunnableLanguage = "python" | "r" | "julia";

const RUNNABLE: Record<string, RunnableLanguage> = {
  python: "python",
  python3: "python",
  py: "python",
  ipython: "python",
  r: "r",
  julia: "julia",
  jl: "julia",
};

/** The research runtime of a fence's language, if it is runnable. */
export function runnableLanguage(language: string | null): RunnableLanguage | null {
  return language ? (RUNNABLE[language.toLowerCase()] ?? null) : null;
}

const MONACO_IDS: Record<string, string> = {
  python: "python",
  python3: "python",
  py: "python",
  ipython: "python",
  r: "r",
  julia: "julia",
  jl: "julia",
  javascript: "javascript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  typescript: "typescript",
  ts: "typescript",
  tsx: "typescript",
  sh: "shell",
  bash: "shell",
  shell: "shell",
  zsh: "shell",
  powershell: "powershell",
  ps1: "powershell",
  pwsh: "powershell",
  c: "cpp",
  cpp: "cpp",
  "c++": "cpp",
  cc: "cpp",
  h: "cpp",
  hpp: "cpp",
  rust: "rust",
  rs: "rust",
  sql: "sql",
  xml: "xml",
  svg: "xml",
  html: "html",
  css: "css",
  yaml: "yaml",
  yml: "yaml",
  markdown: "markdown",
  md: "markdown",
  java: "java",
  go: "go",
  ini: "ini",
  // Exposition's own languages, highlighted when the editor has registered them.
  latex: "latex",
  tex: "latex",
  typst: "typst",
  typ: "typst",
  lean: "lean",
  lean4: "lean",
  json: "json",
  jsonc: "json",
  toml: "toml",
  bibtex: "bibtex",
  bib: "bibtex",
};

/** Monaco language id for a fence language, or null (plain text). */
export function monacoLanguage(language: string | null): string | null {
  return language ? (MONACO_IDS[language.toLowerCase()] ?? null) : null;
}

/** The fence language from a hast `className` (`language-python` → `python`). */
export function fenceLanguage(className: unknown): string | null {
  const classes = Array.isArray(className) ? className : typeof className === "string" ? className.split(/\s+/) : [];
  for (const c of classes) {
    if (typeof c === "string" && c.startsWith("language-")) return c.slice("language-".length) || null;
  }
  return null;
}

/** CSS class suffix for a Monaco token type (`keyword.python` → `keyword`). */
export function tokenClass(type: string): string | null {
  const head = type.split(".")[0] ?? "";
  switch (head) {
    case "comment":
    case "keyword":
    case "string":
    case "number":
    case "type":
    case "tag":
    case "delimiter":
    case "operator":
    case "invalid":
    case "regexp":
    case "variable":
    case "constant":
    case "predefined":
      return head;
    case "attribute":
      return type.startsWith("attribute.name") ? "attribute" : "string";
    default:
      return null;
  }
}
