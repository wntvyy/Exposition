// Symbol catalog: data in catalog.json (shared with a Rust cross-check of the
// Markdown flags), lookup tables per language, and ranked search.
import data from "./catalog.json";
import type { CatalogEntry, MathLanguage, SymbolId } from "./types";

const ENTRIES: readonly CatalogEntry[] = Object.freeze((data as unknown as { entries: CatalogEntry[] }).entries);

const BY_ID = new Map<SymbolId, CatalogEntry>();
const BY_LATEX = new Map<string, CatalogEntry>();
const BY_TYPST = new Map<string, CatalogEntry>();
const BY_GLYPH = new Map<string, CatalogEntry>();
const INDEX = new Map<CatalogEntry, number>();

ENTRIES.forEach((e, i) => {
  INDEX.set(e, i);
  BY_ID.set(e.id, e);
  if (e.latex && e.id !== "delim.none") {
    for (const s of [e.latex.command, ...(e.latex.alternatives ?? [])]) if (!BY_LATEX.has(s)) BY_LATEX.set(s, e);
  }
  if (e.typst && !BY_TYPST.has(e.typst.name)) BY_TYPST.set(e.typst.name, e);
  if (e.glyph && (e.role === "symbol" || e.role === "delim" || e.role === "bigop") && !BY_GLYPH.has(e.glyph)) BY_GLYPH.set(e.glyph, e);
});

export const allEntries = (): readonly CatalogEntry[] => ENTRIES;
export const entryById = (id: SymbolId): CatalogEntry | undefined => BY_ID.get(id);
/** Entry for a LaTeX spelling (`\leq`, `\le`, `+`, `\not\equiv`, `\mathbb{R}`). */
export const entryByLatex = (spelling: string): CatalogEntry | undefined => BY_LATEX.get(spelling);
/** Entry for a Typst name (`lt.eq`, `alpha`, `hat`). */
export const entryByTypst = (name: string): CatalogEntry | undefined => BY_TYPST.get(name);
/** Symbol-like entry for a Unicode glyph (`≤`). */
export const entryByGlyph = (glyph: string): CatalogEntry | undefined => BY_GLYPH.get(glyph);

/** TeX class of an entry, used for spacing. */
export const classOf = (id: SymbolId): NonNullable<CatalogEntry["mathClass"]> => BY_ID.get(id)?.mathClass ?? "ord";

export function availableIn(entry: CatalogEntry, language: MathLanguage): boolean {
  if (language === "latex") return entry.latex !== null;
  if (language === "typst") return entry.typst !== null;
  return entry.markdown;
}

// ---------------------------------------------------------------- search

interface SearchKey {
  /** Normalized (lowercase) text. */
  key: string;
  /** Original text, for case-exact ties and display. */
  raw: string;
  /** Offsets where words start inside `key`. */
  starts: number[];
  label: string;
}

const norm = (s: string) => s.normalize("NFC").toLowerCase().trim();

function wordStarts(key: string): number[] {
  const out: number[] = [];
  for (let i = 1; i < key.length; i++) {
    const prev = key[i - 1] ?? "";
    if (/[\s.\-_(),/\\{}]/.test(prev) && !/[\s.\-_(),/\\{}]/.test(key[i] ?? "")) out.push(i);
  }
  return out;
}

function keysFor(e: CatalogEntry): SearchKey[] {
  const keys: SearchKey[] = [];
  const add = (raw: string, label: string) => {
    if (!raw) return;
    const key = norm(raw);
    if (!key) return;
    keys.push({ key, raw: raw.trim(), starts: wordStarts(key), label });
  };
  add(e.name, `name ${e.name}`);
  for (const a of e.aliases) add(a, `alias ${a}`);
  if (e.glyph && !e.glyph.startsWith("◌")) add(e.glyph, `glyph ${e.glyph}`);
  if (e.latex) {
    for (const s of [e.latex.command, ...(e.latex.alternatives ?? [])]) {
      add(s, `LaTeX ${s}`);
      if (/^\\[A-Za-z]+$/.test(s)) add(s.slice(1), `LaTeX ${s}`);
    }
  }
  if (e.typst) add(e.typst.name, `Typst ${e.typst.name}`);
  return keys;
}

const SEARCH_INDEX: { entry: CatalogEntry; keys: SearchKey[] }[] = ENTRIES.map((entry) => ({ entry, keys: keysFor(entry) }));

export interface CatalogSearchHit {
  entry: CatalogEntry;
  matched: string;
  available: boolean;
}

/**
 * Ranked search: exact > prefix > word prefix > substring (each tier split by
 * case-exact before case-insensitive), ties by catalog order.
 */
export function search(query: string, options: { language?: MathLanguage; limit?: number; category?: string }): CatalogSearchHit[] {
  const limit = options.limit ?? 50;
  const q = norm(query);
  const raw = query.trim();
  const language = options.language;
  const hits: { entry: CatalogEntry; score: number; index: number; matched: string }[] = [];
  for (const { entry, keys } of SEARCH_INDEX) {
    if (options.category && entry.category !== options.category) continue;
    if (!q) {
      hits.push({ entry, score: 0, index: INDEX.get(entry) ?? 0, matched: "" });
      continue;
    }
    let best = Infinity;
    let matched = "";
    for (const k of keys) {
      let tier: number;
      if (k.key === q) tier = 0;
      else if (k.key.startsWith(q)) tier = 1;
      else if (k.starts.some((s) => k.key.startsWith(q, s))) tier = 2;
      else if (k.key.includes(q)) tier = 3;
      else continue;
      const score = tier * 2 + (tier === 0 && k.raw === raw ? 0 : 1);
      if (score < best) {
        best = score;
        matched = k.label;
      }
    }
    if (best < Infinity) hits.push({ entry, score: best, index: INDEX.get(entry) ?? 0, matched });
  }
  hits.sort((a, b) => a.score - b.score || a.index - b.index);
  return hits.slice(0, limit).map((h) => ({
    entry: h.entry,
    matched: h.matched,
    available: language ? availableIn(h.entry, language) : true,
  }));
}
