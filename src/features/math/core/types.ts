// Contract of the visual equation editor's core (coordinator-owned; lanes
// implement it). The math tree is an editing representation only: the document
// source stays authoritative, and every edit is applied to it as one
// transaction. Nothing here touches the document service.

/** Typesetting syntaxes with equation support. TeX engines (pdfLaTeX, LuaLaTeX) share `latex`. */
export type MathLanguage = "latex" | "typst" | "markdown";

/** A stable semantic symbol id from the catalog (e.g. "rel.leq", "greek.alpha"). */
export type SymbolId = string;

/** A row of math: an ordered sequence of nodes (the unit a caret moves in). */
export interface MathRow {
  kind: "row";
  children: MathNode[];
}

export type MathNode =
  /** One letter/identifier character run typed by the user (italic variable letters). */
  | { kind: "ident"; text: string }
  /** Digits and decimal separator. */
  | { kind: "number"; text: string }
  /** A catalog symbol (operator, relation, arrow, Greek letter, ...). */
  | {
      kind: "symbol";
      id: SymbolId;
      /** LaTeX spelling from the source when it is one of the entry's `latex.alternatives` (e.g. `\le`); LaTeX/Markdown serializers keep it. */
      command?: string;
    }
  /** Upright text inside math (\text{...} / "..." in Typst). */
  | {
      kind: "text";
      text: string;
      /** LaTeX command when not `\text` (`\textrm`, `\mbox`, `\textit`, `\textbf`, `\textnormal`). */
      command?: string;
    }
  | {
      kind: "frac";
      num: MathRow;
      den: MathRow;
      /** Source form to preserve: LaTeX `\dfrac`/`\tfrac`/`\cfrac`/`\binom` (binomial coefficient), Typst `a / b` ("slash"). */
      variant?: "dfrac" | "tfrac" | "cfrac" | "binom" | "slash";
    }
  | { kind: "sqrt"; body: MathRow; index: MathRow | null }
  /** Base with optional sub/superscript (base may be an empty row). */
  | { kind: "scripts"; base: MathRow; sub: MathRow | null; sup: MathRow | null }
  /** Integrals, sums, products, unions, limits... `op` is a catalog id; bounds optional. */
  | {
      kind: "bigop";
      op: SymbolId;
      lower: MathRow | null;
      upper: MathRow | null;
      /** LaTeX `\limits` (true) / `\nolimits` (false) after the operator. */
      limits?: boolean;
    }
  /** Named function: sin, cos, log, lim, max, ... (catalog id). */
  | {
      kind: "func";
      name: SymbolId;
      /** Operator text for "func.operatorname" (`\operatorname{tr}`, Typst `op("tr")`). */
      text?: string;
    }
  /** Paired scalable delimiters around a body; delimiters are catalog ids ("delim.paren.l", "delim.none"). */
  | {
      kind: "delim";
      left: SymbolId;
      right: SymbolId;
      body: MathRow;
      /**
       * Source form: absent = scalable (`\left( ... \right)`, Typst natural/`lr`); "plain" = fixed-size
       * characters (`( ... )` in LaTeX); "func" = Typst `abs()`/`norm()`/`floor()`/`ceil()`.
       */
      form?: "plain" | "func";
    }
  /** Accents and decorations: hat, bar, vec, dot, tilde, overline, underbrace... */
  | { kind: "accent"; accent: SymbolId; body: MathRow }
  /** Alphabet/style: mathbb, mathcal, mathbf, mathrm, mathfrak, ... */
  | { kind: "font"; font: SymbolId; body: MathRow }
  /** Matrix-like grids. `fence` selects pmatrix/bmatrix/vmatrix/... ("delim.paren", "delim.bracket", "delim.vert", "none"). */
  | {
      kind: "matrix";
      fence: string;
      rows: MathRow[][];
      /** Typst `vec(...)` (one column). */
      form?: "vec";
    }
  /** Piecewise definition: each row is [value, condition]. */
  | { kind: "cases"; rows: [MathRow, MathRow][] }
  /** Aligned multi-line expression: rows of cells joined by alignment points. */
  | {
      kind: "aligned";
      rows: MathRow[][];
      /**
       * True for rows written directly in the equation (LaTeX `align`/`gather`/... environment bodies,
       * Typst top-level `&`/`\`), false/absent for a nested `aligned`-style environment.
       */
      bare?: boolean;
      /** LaTeX environment when nested: "aligned" (default), "gathered", "split". */
      environment?: string;
    }
  /**
   * Source the adapter does not understand (custom macros, unsupported
   * constructs). Serialized back byte-for-byte; never edited or flattened.
   */
  | { kind: "opaque"; source: string; language: MathLanguage; reason: string };

/** Where an equation sits in the document source (UTF-16 offsets, like the document service). */
export interface EquationRange {
  /** Whole construct including delimiters (`$...$`, `\begin{equation}...\end{equation}`). */
  start: number;
  end: number;
  /** The math content between the delimiters. */
  contentStart: number;
  contentEnd: number;
  display: boolean;
  /** Environment or delimiter style to preserve on rewrite ("$", "$$", "\\[", "equation", "align*", "typst-inline", "typst-block"). */
  style: string;
}

/** What is at a source offset. */
export type SourceContext =
  | { kind: "math"; equation: EquationRange }
  | { kind: "text" }
  /** Comments, code blocks, raw/verbatim, preamble: no equation may be inserted here. */
  | { kind: "unavailable"; reason: string };

export interface ParseResult {
  row: MathRow;
  /** True when every part was understood (no opaque segments). */
  complete: boolean;
}

/** Extra document configuration a serialization needs (e.g. LaTeX packages). */
export interface Requirement {
  /** e.g. { kind: "latexPackage", name: "amssymb" } */
  kind: "latexPackage";
  name: string;
}

export interface SerializeResult {
  source: string;
  requirements: Requirement[];
  /** Nodes this language cannot express (named by catalog id or node kind), with reasons. */
  unsupported: { what: string; reason: string }[];
}

/** One adapter per language; pure functions over strings. */
export interface MathLanguageAdapter {
  language: MathLanguage;
  /** Every equation in a document's text, in order. */
  equations(text: string): EquationRange[];
  /** Context at an offset (inside an equation's content, ordinary text, or unavailable). */
  contextAt(text: string, offset: number): SourceContext;
  /** Parse equation content (between delimiters). Unknown parts become opaque nodes. */
  parse(content: string): ParseResult;
  /** Serialize content (without delimiters). */
  serialize(row: MathRow): SerializeResult;
  /** Wrap content in new delimiters for insertion in text. */
  wrap(content: string, display: boolean): string;
  /** Packages/configuration already present in the document (LaTeX preamble), for requirement checks. */
  satisfied(text: string, requirement: Requirement): boolean;
  /** Text edit that adds a requirement to the document (e.g. a \usepackage line), or null if it cannot be added automatically. */
  addRequirement(text: string, requirement: Requirement): { offset: number; insert: string } | null;
}

/**
 * A path from the root row to a row (alternating node index and slot name).
 * Slot names: frac "num"/"den"; sqrt "index"/"body"; scripts "base"/"sub"/"sup"; bigop "lower"/"upper";
 * delim/accent/font "body"; matrix/cases/aligned "r<row>c<col>" (e.g. "r0c1"). A path ending in
 * [nodeIndex, "text"] puts the caret inside a text node: the offset then counts UTF-16 units of its text.
 */
export type SlotPath = (number | string)[];

/** Caret inside the editing tree: a row and a position between its children. */
export interface MathCaret {
  path: SlotPath;
  offset: number;
  /** Optional selection anchor in the same row. */
  anchor?: number;
}

export interface EditorState {
  root: MathRow;
  caret: MathCaret;
}

/** Templates the panel offers; each creates a structure with editable slots. */
export type TemplateId =
  | "frac"
  | "sqrt"
  | "root"
  | "sup"
  | "sub"
  | "subsup"
  | "integral"
  | "sum"
  | "product"
  | "limit"
  | "parens"
  | "brackets"
  | "braces"
  | "abs"
  | "norm"
  | "matrix"
  | "cases"
  | "aligned"
  | "text";

export interface CatalogEntry {
  id: SymbolId;
  /** Readable name: "less than or equal to". */
  name: string;
  category: string;
  /** Unicode glyph(s) for display and glyph search. */
  glyph: string;
  /** Search synonyms: "leq", "le", "<=", "less or equal". */
  aliases: string[];
  /** How the entry is placed in the tree. */
  role: "symbol" | "bigop" | "func" | "accent" | "font" | "delim";
  /**
   * `command` is the canonical spelling; `alternatives` are other spellings that parse to this entry
   * (`\le`, `\not\leq`); `markdown` is the spelling the Markdown print profile accepts when it does
   * not accept `command` (e.g. `\not\leq` for `\nleq`).
   */
  latex: { command: string; package?: string; alternatives?: string[]; markdown?: string } | null;
  typst: { name: string } | null;
  /** Supported by the Markdown print profile's math translator. */
  markdown: boolean;
  /** TeX math class, used for spacing and for the editing model ("bin" / "rel" get spaces). */
  mathClass?: "ord" | "bin" | "rel" | "op" | "open" | "close" | "punct";
}
