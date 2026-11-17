// Public surface of the equation core (contract; lane EQ-A implements every
// function, lane EQ-B and the editor/page integration only import from here).
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

const pending = (name: string): never => {
  throw new Error(`math core: ${name} is not implemented yet`);
};

// ---- languages

/** The math language of a document path, or null when it has no equation support. */
export function languageForPath(path: string): MathLanguage | null {
  const lower = path.toLowerCase();
  if (lower.endsWith(".tex")) return "latex";
  if (lower.endsWith(".typ")) return "typst";
  if (lower.endsWith(".md") || lower.endsWith(".markdown")) return "markdown";
  return null;
}

export function adapterFor(language: MathLanguage): MathLanguageAdapter {
  return pending(`adapterFor(${language})`);
}

// ---- symbol catalog

export function catalog(): readonly CatalogEntry[] {
  return pending("catalog");
}

export function catalogEntry(id: SymbolId): CatalogEntry | undefined {
  return pending(`catalogEntry(${id})`);
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
  return pending(`searchCatalog(${query}, ${JSON.stringify(options)})`);
}

// ---- editing model (pure: every operation returns a new state)

export function createState(root?: MathRow): EditorState {
  return pending(`createState(${root ? "row" : ""})`);
}

export type Motion = "left" | "right" | "up" | "down" | "nextSlot" | "prevSlot" | "home" | "end";

export function move(state: EditorState, motion: Motion, extend?: boolean): EditorState {
  return pending(`move(${motion}, ${extend}, ${state.caret.offset})`);
}

/** Type characters: letters become identifiers, digits numbers; `^`, `_`, `/` start templates. */
export function typeText(state: EditorState, text: string): EditorState {
  return pending(`typeText(${text}, ${state.caret.offset})`);
}

export function insertSymbol(state: EditorState, id: SymbolId): EditorState {
  return pending(`insertSymbol(${id}, ${state.caret.offset})`);
}

/** Insert a structure at the caret; a selection becomes its first slot where structurally valid. */
export function insertTemplate(state: EditorState, template: TemplateId, options?: { rows?: number; cols?: number }): EditorState {
  return pending(`insertTemplate(${template}, ${JSON.stringify(options)}, ${state.caret.offset})`);
}

export function deleteBackward(state: EditorState): EditorState {
  return pending(`deleteBackward(${state.caret.offset})`);
}

export function deleteForward(state: EditorState): EditorState {
  return pending(`deleteForward(${state.caret.offset})`);
}

export type GridEdit = "addRowAfter" | "addRowBefore" | "addColAfter" | "addColBefore" | "removeRow" | "removeCol";

/** Row/column editing when the caret is inside a matrix, cases, or aligned cell. */
export function editGrid(state: EditorState, edit: GridEdit): EditorState {
  return pending(`editGrid(${edit}, ${state.caret.offset})`);
}

export function setCaret(state: EditorState, caret: MathCaret): EditorState {
  return pending(`setCaret(${caret.offset}, ${state.caret.offset})`);
}

/** Structural equality ignoring caret: used to skip applying unchanged drafts. */
export function sameMath(a: MathRow, b: MathRow): boolean {
  return pending(`sameMath(${a.children.length}, ${b.children.length})`);
}

/** LaTeX for the local KaTeX preview (approximate; the document's compile is authoritative). */
export function previewLatex(row: MathRow): string {
  return pending(`previewLatex(${row.children.length})`);
}
