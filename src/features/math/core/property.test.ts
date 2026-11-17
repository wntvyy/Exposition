// Property: for random trees, every form a writer emits reads back as the same tree
// (serialize → parse → sameMath), per language. Catalog entries whose spelling is
// shared with another entry in a language (documented merges, see MERGES) are left
// out of the random symbols and checked separately.
import { describe, expect, it } from "vitest";
import { adapterFor, catalog, createState, insertSymbol, sameMath, type CatalogEntry, type MathLanguage, type MathNode, type MathRow } from "./index";

function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

/**
 * Entries that share their written form with another entry in a language, so they read
 * back as that entry. They render the same; the distinction exists only in the catalog.
 */
const MERGES: Record<MathLanguage, Record<string, string>> = {
  latex: {},
  typst: {
    "letter.hslash": "letter.hbar",
    "op.smallsetminus": "op.setminus",
    "logic.implies": "arrow.r.double.long",
    "logic.impliedby": "arrow.l.double.long",
    "logic.iff": "arrow.lr.double.long",
    "calc.primecmd": "calc.prime",
    "delim.vert.cmd": "delim.vert",
    "delim.Vert.cmd": "delim.Vert",
    "accent.widehat": "accent.hat",
    "accent.widetilde": "accent.tilde",
    "accent.overrightarrow": "accent.vec",
    "font.bm": "font.boldsymbol",
    "delim.vert.l": "delim.vert",
    "delim.vert.r": "delim.vert",
    "delim.Vert.l": "delim.Vert",
    "delim.Vert.r": "delim.Vert",
    "punct.colon.cmd": "punct.colon",
    "misc.triangle": "op.bigtriangleup",
    "misc.triangledown": "op.bigtriangledown",
  },
  markdown: {},
};

const row = (children: MathNode[]): MathRow => ({ kind: "row", children });

/** The tree `insertSymbol` makes for an entry. */
const inserted = (id: string) => insertSymbol(createState(), id).root;

function pools(language: MathLanguage) {
  const a = adapterFor(language);
  const usable = (e: CatalogEntry) => {
    if (language === "latex" && !e.latex) return false;
    if (language === "typst" && !e.typst) return false;
    if (language === "markdown" && !e.markdown) return false;
    if (MERGES[language][e.id]) return false;
    const tree = inserted(e.id);
    return a.serialize(tree).unsupported.length === 0;
  };
  const all = catalog().filter(usable);
  return {
    // Unpaired brackets and the period (part of numbers) are exercised by other tests.
    symbols: all.filter((e) => e.role === "symbol" && !["open", "close"].includes(e.mathClass ?? "") && e.id !== "punct.period" && e.id !== "delim.vert"),
    funcs: all.filter((e) => e.role === "func" && e.id !== "func.operatorname"),
    bigops: all.filter((e) => e.role === "bigop"),
    accents: all.filter((e) => e.role === "accent"),
    fonts: all.filter((e) => e.role === "font"),
  };
}

function generator(language: MathLanguage, seed: number) {
  const next = rng(seed);
  const pick = <T,>(xs: readonly T[]) => xs[Math.floor(next() * xs.length)] as T;
  const p = pools(language);
  const texts = ["if", "a b", 'say "hi"', "50%", "x\\y", "tab\tz"];
  const delims: [string, string, MathNode["kind"] extends never ? never : "plain" | undefined][] =
    language === "typst"
      ? [
          ["delim.paren.l", "delim.paren.r", undefined],
          ["delim.bracket.l", "delim.bracket.r", undefined],
          ["delim.brace.l", "delim.brace.r", undefined],
          ["delim.vert", "delim.vert", undefined],
          ["delim.Vert", "delim.Vert", undefined],
          ["delim.floor.l", "delim.floor.r", undefined],
          ["delim.angle.l", "delim.angle.r", undefined],
        ]
      : [
          ["delim.paren.l", "delim.paren.r", "plain"],
          ["delim.bracket.l", "delim.bracket.r", "plain"],
          ["delim.brace.l", "delim.brace.r", "plain"],
          ["delim.paren.l", "delim.paren.r", undefined],
          ["delim.angle.l", "delim.angle.r", undefined],
          ["delim.vert", "delim.vert", undefined],
          ["delim.none", "delim.vert", undefined],
        ];
  const variants: (undefined | "dfrac" | "tfrac" | "binom" | "slash")[] = language === "typst" ? [undefined, "slash", "binom"] : [undefined, "dfrac", "tfrac", "binom"];
  const fences = ["delim.paren", "delim.bracket", "delim.brace", "delim.vert", "delim.Vert", "none"];

  const node = (depth: number): MathNode => {
    const r = next();
    if (depth >= 3 || r < 0.45) {
      const k = next();
      if (k < 0.25) return { kind: "ident", text: pick([..."abcxyzABQ"]) };
      if (k < 0.4) return { kind: "number", text: pick([..."0123456789"]) };
      if (k < 0.75) return { kind: "symbol", id: pick(p.symbols).id };
      if (k < 0.85) return { kind: "func", name: pick(p.funcs).id };
      if (k < 0.92) return { kind: "bigop", op: pick(p.bigops).id, lower: null, upper: null };
      return { kind: "text", text: pick(texts) };
    }
    const sub = () => rowOf(depth + 1);
    const opt = () => (next() < 0.5 ? sub() : null);
    switch (Math.floor(next() * 10)) {
      case 0: {
        const variant = pick(variants);
        return variant ? { kind: "frac", num: sub(), den: sub(), variant } : { kind: "frac", num: sub(), den: sub() };
      }
      case 1:
        return { kind: "sqrt", body: sub(), index: opt() };
      case 2: {
        const which = next();
        return { kind: "scripts", base: row(next() < 0.8 ? [{ kind: "ident", text: pick([..."xyf"]) }] : []), sub: which < 0.66 ? sub() : null, sup: which >= 0.33 ? sub() : null };
      }
      case 3:
        return { kind: "bigop", op: pick(p.bigops).id, lower: opt(), upper: opt() };
      case 4: {
        const [left, right, form] = pick(delims);
        return form ? { kind: "delim", left, right, body: sub(), form } : { kind: "delim", left, right, body: sub() };
      }
      case 5:
        return { kind: "accent", accent: pick(p.accents).id, body: sub() };
      case 6:
        return { kind: "font", font: pick(p.fonts).id, body: row([{ kind: "ident", text: pick([..."xyzw"]) }]) };
      case 7: {
        const rows = 1 + Math.floor(next() * 2);
        const cols = 1 + Math.floor(next() * 2);
        return { kind: "matrix", fence: pick(fences), rows: Array.from({ length: rows }, () => Array.from({ length: cols }, () => sub())) };
      }
      case 8:
        return { kind: "cases", rows: Array.from({ length: 1 + Math.floor(next() * 2) }, (): [MathRow, MathRow] => [sub(), sub()]) };
      default:
        if (language === "typst") return { kind: "sqrt", body: sub(), index: null };
        return { kind: "aligned", rows: Array.from({ length: 1 + Math.floor(next() * 2) }, () => [sub(), sub()]) };
    }
  };
  const rowOf = (depth: number): MathRow => row(Array.from({ length: Math.floor(next() * (depth === 0 ? 5 : 3)) }, () => node(depth)));
  return () => rowOf(0);
}

describe("every written form reads back as the same tree", () => {
  for (const language of ["latex", "typst", "markdown"] as const) {
    it(`${language}: serialize → parse → sameMath on random trees`, () => {
      const a = adapterFor(language);
      const tree = generator(language, 977 + language.length);
      const failures: string[] = [];
      for (let i = 0; i < 1500; i++) {
        const t = tree();
        const out = a.serialize(t);
        if (out.unsupported.length) continue;
        const back = a.parse(out.source).row;
        if (!sameMath(back, t)) failures.push(`${out.source}\n  ${JSON.stringify(t)}\n  ${JSON.stringify(back)}`);
      }
      expect(failures.slice(0, 3)).toEqual([]);
    });

    it(`${language}: every catalog entry reads back as itself or its documented merge`, () => {
      const a = adapterFor(language);
      const wrong: string[] = [];
      for (const e of catalog()) {
        const tree = inserted(e.id);
        const out = a.serialize(tree);
        if (out.unsupported.length) continue;
        const back = a.parse(out.source).row;
        if (sameMath(back, tree)) continue;
        const merged = MERGES[language][e.id];
        if (merged && JSON.stringify(back).includes(`"${merged}"`)) continue;
        wrong.push(`${e.id}: ${out.source} -> ${JSON.stringify(back)}`);
      }
      expect(wrong).toEqual([]);
    });
  }
});
