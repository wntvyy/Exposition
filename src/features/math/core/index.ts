// Public surface of the equation core (contract; lane EQ-A implements every
// function, lane EQ-B and the editor/page integration only import from here).
import { allEntries, entryById, search } from "./catalog";
import * as editor from "./editor";
import { latexAdapter } from "./latex";
import { markdownAdapter } from "./markdown";
import { sameMath as sameMathImpl } from "./tree";
import { typstAdapter } from "./typst";
import type {
  CatalogEntry,
  EditorState,
  MathCaret,
  MathLanguage,
  MathLanguageAdapter,
  MathRow,
  SymbolId,
  TemplateId,
} from "./types";

export type * from "./types";

// ---- languages

/** The math language of a document path, or null when it has no equation support. */
export function languageForPath(path: string): MathLanguage | null {
  const lower = path.toLowerCase();
  if (lower.endsWith(".tex")) return "latex";
  if (lower.endsWith(".typ")) return "typst";
  if (lower.endsWith(".md") || lower.endsWith(".markdown")) return "markdown";
  return null;
}

const ADAPTERS: Record<MathLanguage, MathLanguageAdapter> = {
  latex: latexAdapter,
  typst: typstAdapter,
  markdown: markdownAdapter,
};

export function adapterFor(language: MathLanguage): MathLanguageAdapter {
  return ADAPTERS[language];
}

/**
 * New equation content that keeps the old content's surrounding layout: its
 * leading and trailing whitespace, and the indentation of its first line on
 * every following line (for `\begin{align} ... \end{align}` bodies, `$ ... $`
 * Typst blocks, `$$` blocks on their own lines).
 */
export function withContentLayout(oldContent: string, newSource: string): string {
  const lead = /^\s*/.exec(oldContent)?.[0] ?? "";
  let trail = lead.length === oldContent.length ? "" : (/\s*$/.exec(oldContent)?.[0] ?? "");
  const indent = lead.includes("\n") ? lead.slice(lead.lastIndexOf("\n") + 1) : "";
  const nl = oldContent.includes("\r\n") || (!oldContent.includes("\n") && newSource.includes("\r\n")) ? "\r\n" : "\n";
  // Source ending with a line break (after a `%` or `//` comment) needs exactly one line end
  // before the closing delimiter; no line of the result may be white space only (a blank
  // line inside display math is a TeX error).
  const needsBreak = /\n[ \t]*$/.test(newSource);
  const body = newSource
    .replace(/\s+$/, "")
    .split(/\r?\n/)
    .map((line, i) => (i === 0 || line.trim() === "" ? line.trimEnd() : `${indent}${line}`))
    .join(nl);
  if (needsBreak && !trail.includes("\n")) trail = `${nl}${trail}`;
  return `${lead}${body}${trail}`;
}

// ---- symbol catalog

export function catalog(): readonly CatalogEntry[] {
  return allEntries();
}

export function catalogEntry(id: SymbolId): CatalogEntry | undefined {
  return entryById(id);
}

export interface SearchHit {
  entry: CatalogEntry;
  /** Why it matched, for display ("alias leq", "glyph ≤"). */
  matched: string;
  /** Available in the requested language. */
  available: boolean;
}

/**
 * Ranked catalog search over names, aliases, glyphs, and command names:
 * exact matches first, then prefixes, then substrings; ties by catalog order.
 */
export function searchCatalog(query: string, options: { language?: MathLanguage; limit?: number; category?: string }): SearchHit[] {
  return search(query, options);
}

// ---- editing model (pure: every operation returns a new state)

export function createState(root?: MathRow): EditorState {
  return editor.createState(root);
}

export type Motion = "left" | "right" | "up" | "down" | "nextSlot" | "prevSlot" | "home" | "end";

export function move(state: EditorState, motion: Motion, extend?: boolean): EditorState {
  return editor.move(state, motion, extend);
}

/** Type characters: letters become identifiers, digits numbers; `^`, `_`, `/` start templates. */
export function typeText(state: EditorState, text: string): EditorState {
  return editor.typeText(state, text);
}

export function insertSymbol(state: EditorState, id: SymbolId): EditorState {
  return editor.insertSymbol(state, id);
}

/** Insert a structure at the caret; a selection becomes its first slot where structurally valid. */
export function insertTemplate(state: EditorState, template: TemplateId, options?: { rows?: number; cols?: number }): EditorState {
  return editor.insertTemplate(state, template, options);
}

export function deleteBackward(state: EditorState): EditorState {
  return editor.deleteBackward(state);
}

export function deleteForward(state: EditorState): EditorState {
  return editor.deleteForward(state);
}

export type GridEdit = "addRowAfter" | "addRowBefore" | "addColAfter" | "addColBefore" | "removeRow" | "removeCol";

/** Row/column editing when the caret is inside a matrix, cases, or aligned cell. */
export function editGrid(state: EditorState, edit: GridEdit): EditorState {
  return editor.editGrid(state, edit);
}

export function setCaret(state: EditorState, caret: MathCaret): EditorState {
  return editor.setCaret(state, caret);
}

/** Structural equality ignoring caret: used to skip applying unchanged drafts. */
export function sameMath(a: MathRow, b: MathRow): boolean {
  return sameMathImpl(a, b);
}

/** LaTeX for the local KaTeX preview (approximate; the document's compile is authoritative). */
export function previewLatex(row: MathRow): string {
  return editor.previewLatex(row);
}
