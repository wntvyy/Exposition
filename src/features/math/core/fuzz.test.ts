// Deterministic random inputs: adapters and the editing model must never throw,
// round trips must be stable, and the caret must always stay valid.
import { describe, expect, it } from "vitest";
import {
  adapterFor,
  createState,
  deleteBackward,
  deleteForward,
  editGrid,
  insertSymbol,
  insertTemplate,
  move,
  previewLatex,
  sameMath,
  setCaret,
  typeText,
  type EditorState,
  type MathLanguage,
} from "./index";

function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

const pieces: Record<MathLanguage, string[]> = {
  latex: ["x", "y", "2", "+", "-", "=", "^", "_", "{", "}", "(", ")", "[", "]", "\\frac", "\\sqrt", "\\left(", "\\right)", "\\alpha", "\\leq", "\\sum", "\\int", "\\text{a b}", "\\mathbb", "\\hat", " ", "&", "\\\\", "\\begin{pmatrix}", "\\end{pmatrix}", "\\begin{cases}", "\\end{cases}", "\\label{q}", "\\foo", "%c\n", "'", ",", "\\,", "\\not", "\\{", "\\}", "|", "\\lim", "\\operatorname{f}", "\\left.", "\\right|", "\\begin{aligned}", "\\end{aligned}", "\\limits", ".", "\\dfrac", "\\binom", "\\mathrm{d}", "\\text{%}", "\\sqrt[", "]"],
  typst: ["x", "y", "2", "+", "-", "=", "^", "_", "(", ")", "[", "]", "frac(", "sqrt(", ",", ";", "alpha", "<=", "sum", "integral", '"a b"', "bb(", "hat(", " ", "&", "\\ ", "mat(", "cases(", "#h(1em)", "foo", "// c\n", "'", "/", "lr(", "abs(", 'op("f")', "lim", "!", "attach(", "t: ", "b: ", 'delim: "["', "vec(", "{", "}", "1.5", "dif", "->", "...", '""'],
  markdown: ["x", "2", "+", "^", "_", "{", "}", "\\frac", "\\alpha", "\\le", " ", "\\sqrt", "(", ")", "%", "\\label{q}", "\\nleq", "\\cancel", "&", "\\\\", "\\begin{matrix}", "\\end{matrix}", "|", "\\}", "\\{", "[", "]"],
};

describe("adapters on random content", () => {
  for (const language of ["latex", "typst", "markdown"] as const) {
    it(`${language}: never throws, round trips are stable`, () => {
      const a = adapterFor(language);
      const next = rng(language.length * 7919);
      for (let i = 0; i < 1500; i++) {
        const n = 1 + Math.floor(next() * 12);
        let src = "";
        for (let k = 0; k < n; k++) src += pieces[language][Math.floor(next() * pieces[language].length)];
        const first = a.parse(src);
        const out = a.serialize(first.row);
        const second = a.parse(out.source);
        expect(sameMath(first.row, second.row), `${JSON.stringify(src)} -> ${JSON.stringify(out.source)}`).toBe(true);
        expect(a.serialize(second.row).source, JSON.stringify(src)).toBe(out.source);
        expect(typeof previewLatex(first.row)).toBe("string");
      }
    });
  }

  it("locators never throw and return ordered, well-formed ranges", () => {
    const next = rng(42);
    const chars = ["$", "$$", "\\", "%", "\n", "`", "```\n", "#", "[", "]", "{", "}", "(", ")", "/", "*", '"', "x", " ", "\\begin{equation}", "\\end{equation}", "\\(", "\\)", "\\[", "\\]", "\n\n", "---\n", "<!--", "-->"];
    for (const language of ["latex", "typst", "markdown"] as const) {
      const a = adapterFor(language);
      for (let i = 0; i < 800; i++) {
        let text = "";
        const n = Math.floor(next() * 30);
        for (let k = 0; k < n; k++) text += chars[Math.floor(next() * chars.length)];
        const eqs = a.equations(text);
        let last = -1;
        for (const e of eqs) {
          expect(e.start).toBeGreaterThanOrEqual(last);
          expect(e.start <= e.contentStart && e.contentStart <= e.contentEnd && e.contentEnd <= e.end && e.end <= text.length).toBe(true);
          last = e.end;
        }
        for (let o = 0; o <= text.length; o += 3) a.contextAt(text, o);
      }
    }
  });
});

describe("editing model on random operation sequences", () => {
  it("never throws and keeps a valid caret", () => {
    const next = rng(2026);
    const templates = ["frac", "sqrt", "root", "sup", "sub", "subsup", "integral", "sum", "product", "limit", "parens", "brackets", "braces", "abs", "norm", "matrix", "cases", "aligned", "text"] as const;
    const motions = ["left", "right", "up", "down", "nextSlot", "prevSlot", "home", "end"] as const;
    const symbols = ["greek.alpha", "rel.leq", "bigop.sum", "func.sin", "func.lim", "accent.hat", "font.bb", "delim.paren.l", "delim.vert", "arrow.r", "func.operatorname", "delim.none", "punct.comma"];
    const grid = ["addRowAfter", "addRowBefore", "addColAfter", "addColBefore", "removeRow", "removeCol"] as const;
    for (let run = 0; run < 150; run++) {
      let s: EditorState = createState();
      for (let step = 0; step < 40; step++) {
        const r = next();
        const pick = <T,>(xs: readonly T[]) => xs[Math.floor(next() * xs.length)] as T;
        if (r < 0.3) s = typeText(s, pick(["a", "2", "^", "_", "/", "(", ")", "+", "=", " ", "x1", "'", "[", "]", "{", "}", ".", "|", ","]));
        else if (r < 0.45) s = insertTemplate(s, pick(templates), { rows: 1 + Math.floor(next() * 3), cols: 1 + Math.floor(next() * 3) });
        else if (r < 0.7) s = move(s, pick(motions), next() < 0.2);
        else if (r < 0.8) s = next() < 0.6 ? deleteBackward(s) : deleteForward(s);
        else if (r < 0.9) s = insertSymbol(s, pick(symbols));
        else s = editGrid(s, pick(grid));
        // The caret is valid when setCaret leaves it unchanged.
        expect(setCaret(s, s.caret).caret).toEqual(s.caret);
      }
      // Whatever was built serializes in every language to source that reads back stably.
      for (const language of ["latex", "typst", "markdown"] as const) {
        const a = adapterFor(language);
        const back = a.parse(a.serialize(s.root).source).row;
        const again = a.serialize(back).source;
        expect(sameMath(a.parse(again).row, back), `${language}: ${again}`).toBe(true);
      }
    }
  });
});
