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
  withContentLayout,
  type EditorState,
  type MathNode,
  type Motion,
} from "./index";

const latex = adapterFor("latex");
const typst = adapterFor("typst");
const src = (s: EditorState) => latex.serialize(s.root).source;
const moves = (s: EditorState, ...ms: Motion[]) => ms.reduce((acc, m) => move(acc, m), s);
const fromLatex = (s: string) => createState(latex.parse(s).row);

describe("typing and templates", () => {
  it("builds a nested fraction with a root and scripts", () => {
    let s = createState();
    expect(s.caret).toEqual({ path: [], offset: 0 });
    s = insertTemplate(s, "frac");
    expect(s.caret).toEqual({ path: [0, "num"], offset: 0 });
    s = typeText(s, "x^2");
    expect(s.caret).toEqual({ path: [0, "num", 0, "sup"], offset: 1 });
    s = move(s, "right"); // out of the superscript
    expect(s.caret).toEqual({ path: [0, "num"], offset: 1 });
    s = typeText(s, "+1");
    s = move(s, "down");
    expect(s.caret).toEqual({ path: [0, "den"], offset: 0 });
    s = insertTemplate(s, "sqrt");
    s = typeText(s, "y_1");
    expect(src(s)).toBe("\\frac{x^2 + 1}{\\sqrt{y_1}}");
    expect(typst.serialize(s.root).source).toBe("frac(x^2 + 1, sqrt(y_1))");
    // Tab leaves the innermost slots in document order, then the structure.
    s = move(s, "nextSlot");
    expect(s.caret).toEqual({ path: [], offset: 1 });
    s = typeText(s, "=z");
    expect(src(s)).toBe("\\frac{x^2 + 1}{\\sqrt{y_1}} = z");
  });

  it("turns typed `/` into a fraction over the previous atom or digit run", () => {
    let s = typeText(createState(), "12/3");
    expect(src(s)).toBe("\\frac{12}{3}");
    expect(s.caret).toEqual({ path: [0, "den"], offset: 1 });
    s = typeText(createState(), "a+/");
    expect(s.root.children[2]).toMatchObject({ kind: "frac", num: { children: [] } });
    expect(s.caret).toEqual({ path: [2, "num"], offset: 0 });
    s = typeText(createState(), "(a+b)/2");
    expect(src(s)).toBe("\\frac{(a + b)}{2}");
  });

  it("puts scripts on the previous atom, limits on operators", () => {
    let s = typeText(createState(), "x_i^2");
    expect(src(s)).toBe("x_i^2");
    expect(src(typeText(createState(), "x^2_i"))).toBe("x_i^2");
    // Inside a non-empty slot that is not at its end, `^` attaches to the previous atom.
    let inner = typeText(createState(), "x_ab");
    inner = move(inner, "left");
    inner = typeText(inner, "^");
    expect(inner.caret.path).toEqual([0, "sub", 0, "sup"]);
    s = insertSymbol(createState(), "bigop.sum");
    s = typeText(s, "_k");
    s = move(s, "right");
    expect(s.caret).toEqual({ path: [], offset: 1 });
    s = moves(s, "left", "left");
    expect(s.caret.path).toEqual([0, "lower"]);
    s = setCaret(s, { path: [], offset: 1 });
    s = typeText(s, "^n");
    expect(src(s)).toBe("\\sum_k^n");
    s = insertSymbol(createState(), "func.lim");
    s = typeText(s, "_x");
    expect(s.root.children[0]).toMatchObject({ kind: "bigop", op: "func.lim" });
    s = typeText(createState(), "^");
    expect(s.root.children[0]).toMatchObject({ kind: "scripts", base: { children: [] } });
    expect(s.caret).toEqual({ path: [0, "sup"], offset: 0 });
    s = typeText(createState(), "a=^");
    expect(s.root.children[2]).toMatchObject({ kind: "scripts", base: { children: [] } });
  });

  it("opens and steps out of typed brackets", () => {
    let s = typeText(createState(), "f(x");
    expect(s.caret).toEqual({ path: [1, "body"], offset: 1 });
    s = typeText(s, ")+1");
    expect(src(s)).toBe("f(x) + 1");
    expect(s.caret).toEqual({ path: [], offset: 4 });
    s = typeText(createState(), "x)");
    expect(s.root.children[1]).toEqual({ kind: "symbol", id: "delim.paren.r" });
  });

  it("wraps a selection into the first valid slot", () => {
    let s = typeText(createState(), "a+b");
    s = move(s, "home");
    s = move(s, "end", true);
    expect(s.caret).toEqual({ path: [], offset: 3, anchor: 0 });
    const sel = s;
    s = insertTemplate(sel, "parens");
    expect(src(s)).toBe("\\left( a + b \\right)");
    expect(s.caret).toEqual({ path: [0, "body"], offset: 3 });
    s = insertTemplate(sel, "frac");
    expect(src(s)).toBe("\\frac{a + b}{}");
    expect(s.caret).toEqual({ path: [0, "den"], offset: 0 });
    s = insertTemplate(sel, "sup");
    expect(src(s)).toBe("{a + b}^{}");
    expect(s.caret).toEqual({ path: [0, "sup"], offset: 0 });
    s = insertTemplate(sel, "root");
    expect(s.caret).toEqual({ path: [0, "index"], offset: 0 });
    expect(src(s)).toBe("\\sqrt[]{a + b}");
    s = insertSymbol(sel, "accent.hat");
    expect(src(s)).toBe("\\hat{a + b}");
    s = insertSymbol(sel, "font.bb");
    expect(src(s)).toBe("\\mathbb{a + b}");
    s = insertSymbol(sel, "delim.angle.l");
    expect(src(s)).toBe("\\left\\langle a + b \\right\\rangle");
    s = typeText(sel, "(");
    expect(src(s)).toBe("(a + b)");
    // Large operators keep the selection after them.
    s = insertTemplate(sel, "integral");
    expect(src(s)).toBe("\\int_{}^{} a + b");
    expect(s.caret).toEqual({ path: [0, "lower"], offset: 0 });
    // A symbol replaces the selection.
    s = insertSymbol(sel, "greek.alpha");
    expect(src(s)).toBe("\\alpha");
  });

  it("creates every template with editable slots", () => {
    const expected: Record<string, string> = {
      frac: "\\frac{}{}",
      sqrt: "\\sqrt{}",
      root: "\\sqrt[]{}",
      sup: "{}^{}",
      sub: "{}_{}",
      subsup: "{}_{}^{}",
      integral: "\\int_{}^{}",
      sum: "\\sum_{}^{}",
      product: "\\prod_{}^{}",
      limit: "\\lim_{}",
      parens: "\\left(\\right)",
      brackets: "\\left[\\right]",
      braces: "\\left\\{\\right\\}",
      abs: "\\left|\\right|",
      norm: "\\left\\|\\right\\|",
      matrix: "\\begin{pmatrix} & \\\\ & \\end{pmatrix}",
      cases: "\\begin{cases}  \\\\ {} \\end{cases}",
      aligned: "\\begin{aligned} & \\\\ & \\end{aligned}",
      text: "\\text{}",
    };
    for (const [t, out] of Object.entries(expected)) {
      const s = insertTemplate(createState(), t as never);
      expect(src(s), t).toBe(out);
      // The caret is inside the new structure, and the source reads back as the same tree.
      expect(s.caret.path.length, t).toBe(2);
      expect(sameMath(latex.parse(src(s)).row, s.root), t).toBe(true);
    }
  });

  it("edits text nodes character by character", () => {
    let s = insertTemplate(createState(), "text");
    expect(s.caret).toEqual({ path: [0, "text"], offset: 0 });
    s = typeText(s, "if x");
    expect(s.root.children[0]).toEqual({ kind: "text", text: "if x" });
    s = deleteBackward(s);
    s = moves(s, "left", "left");
    expect(s.caret).toEqual({ path: [0, "text"], offset: 1 });
    s = typeText(s, "{");
    expect(src(s)).toBe("\\text{i\\{f }");
    s = moves(s, "end", "right");
    expect(s.caret).toEqual({ path: [], offset: 1 });
    s = move(s, "left");
    expect(s.caret).toEqual({ path: [0, "text"], offset: 4 });
  });

  it("inserts symbols, functions, and operators from the catalog", () => {
    let s = insertSymbol(createState(), "greek.alpha");
    s = insertSymbol(s, "rel.leq");
    s = insertSymbol(s, "func.sin");
    s = typeText(s, "x");
    expect(src(s)).toBe("\\alpha \\leq \\sin x");
    expect(insertSymbol(createState(), "no.such.id").root.children).toEqual([]);
  });
});

describe("caret motion", () => {
  const state = fromLatex("a\\frac{b}{c}d");

  it("walks into and out of structures left to right", () => {
    let s = setCaret(state, { path: [], offset: 0 });
    const trail: unknown[] = [];
    for (let i = 0; i < 8; i++) {
      s = move(s, "right");
      trail.push([...s.caret.path, s.caret.offset]);
    }
    expect(trail).toEqual([[1], [1, "num", 0], [1, "num", 1], [1, "den", 0], [1, "den", 1], [2], [3], [3]]);
    for (let i = 0; i < 7; i++) s = move(s, "left");
    expect(s.caret).toEqual({ path: [], offset: 0 });
  });

  it("moves up and down between related slots", () => {
    let s = setCaret(state, { path: [1, "num"], offset: 1 });
    s = move(s, "down");
    expect(s.caret).toEqual({ path: [1, "den"], offset: 1 });
    s = move(s, "down");
    expect(s.caret).toEqual({ path: [1, "den"], offset: 1 });
    s = move(s, "up");
    expect(s.caret.path).toEqual([1, "num"]);
    let t = fromLatex("x_i^2");
    t = setCaret(t, { path: [0, "base"], offset: 1 });
    expect(move(t, "up").caret.path).toEqual([0, "sup"]);
    expect(move(t, "down").caret.path).toEqual([0, "sub"]);
    let m = fromLatex("\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}");
    m = setCaret(m, { path: [0, "r0c1"], offset: 0 });
    expect(move(m, "down").caret).toEqual({ path: [0, "r1c1"], offset: 0 });
    expect(move(m, "up").caret).toEqual({ path: [0, "r0c1"], offset: 0 });
    // Nested: the innermost structure with a vertical relation wins.
    let n = fromLatex("\\frac{x^2}{y}");
    n = setCaret(n, { path: [0, "num", 0, "sup"], offset: 1 });
    expect(move(n, "down").caret.path).toEqual([0, "den"]);
  });

  it("steps through slots with Tab and Shift-Tab", () => {
    let s = fromLatex("\\frac{\\sqrt{a}}{b} + \\int_0^1");
    s = setCaret(s, { path: [], offset: 0 });
    const order: unknown[] = [];
    for (let i = 0; i < 6; i++) {
      s = move(s, "nextSlot");
      order.push(s.caret.path);
    }
    expect(order).toEqual([[0, "num"], [0, "num", 0, "body"], [0, "den"], [2, "lower"], [2, "upper"], []]);
    s = move(s, "prevSlot");
    expect(s.caret.path).toEqual([2, "upper"]);
  });

  it("extends and collapses selections within a row", () => {
    let s = setCaret(fromLatex("abc"), { path: [], offset: 1 });
    s = move(s, "right", true);
    s = move(s, "right", true);
    expect(s.caret).toEqual({ path: [], offset: 3, anchor: 1 });
    expect(move(s, "left").caret).toEqual({ path: [], offset: 1 });
    expect(move(s, "right").caret).toEqual({ path: [], offset: 3 });
    expect(move(s, "home", true).caret).toEqual({ path: [], offset: 0, anchor: 1 });
  });

  it("clamps invalid carets", () => {
    expect(setCaret(state, { path: [9, "num"], offset: 2 }).caret).toEqual({ path: [], offset: 3 });
    expect(setCaret(state, { path: [1, "den"], offset: 99 }).caret).toEqual({ path: [1, "den"], offset: 1 });
  });
});

describe("deletion", () => {
  it("removes atoms and selects structures with content before deleting them", () => {
    let s = typeText(createState(), "ab");
    s = deleteBackward(s);
    expect(src(s)).toBe("a");
    s = setCaret(fromLatex("x + \\frac{a}{b}"), { path: [], offset: 3 });
    s = deleteBackward(s);
    expect(s.caret).toEqual({ path: [], offset: 3, anchor: 2 });
    expect(src(s)).toBe("x + \\frac{a}{b}");
    s = deleteBackward(s);
    expect(src(s)).toBe("x +");
    s = setCaret(fromLatex("\\frac{a}{b} + x"), { path: [], offset: 0 });
    s = deleteForward(s);
    expect(s.caret).toEqual({ path: [], offset: 0, anchor: 1 });
    s = deleteForward(s);
    expect(src(s)).toBe("+x");
    // Empty structures and opaque source.
    s = deleteBackward(insertTemplate(createState(), "frac"));
    expect(s.root.children).toEqual([]);
    s = setCaret(fromLatex("x \\label{a}"), { path: [], offset: 2 });
    expect(deleteBackward(s).caret.anchor).toBe(1);
  });

  it("removes empty optional slots and unwraps at the start of a first slot", () => {
    // Empty superscript: back to the base.
    let s = typeText(createState(), "x^");
    s = deleteBackward(s);
    expect(s.root.children).toEqual([{ kind: "ident", text: "x" }]);
    expect(s.caret).toEqual({ path: [], offset: 1 });
    // Empty subscript next to a superscript: removed, caret to the end of the base.
    s = setCaret(fromLatex("x_{}^2"), { path: [0, "sub"], offset: 0 });
    s = deleteBackward(s);
    expect(src(s)).toBe("x^2");
    expect(s.caret).toEqual({ path: [0, "base"], offset: 1 });
    // Numerator start: the fraction unwraps.
    s = setCaret(fromLatex("\\frac{a}{b}"), { path: [0, "num"], offset: 0 });
    s = deleteBackward(s);
    expect(src(s)).toBe("ab");
    expect(s.caret).toEqual({ path: [], offset: 0 });
    // Denominator start: to the end of the numerator.
    s = setCaret(fromLatex("\\frac{a}{b}"), { path: [0, "den"], offset: 0 });
    expect(deleteBackward(s).caret).toEqual({ path: [0, "num"], offset: 1 });
    // Delimiters, accents, roots unwrap.
    for (const [input, out] of [["\\left( x \\right)", "x"], ["\\hat{x}", "x"], ["\\sqrt{x}", "x"], ["\\mathbb{R}", "R"]]) {
      expect(src(deleteBackward(setCaret(fromLatex(input as string), { path: [0, "body"], offset: 0 })))).toBe(out);
    }
    // Empty root index is removed.
    s = setCaret(fromLatex("\\sqrt[]{x}"), { path: [0, "index"], offset: 0 });
    expect(src(deleteBackward(s))).toBe("\\sqrt{x}");
    // Empty limit is removed.
    s = setCaret(fromLatex("\\sum_{}^{n}"), { path: [0, "lower"], offset: 0 });
    expect(src(deleteBackward(s))).toBe("\\sum^n");
    // Matrices: leave instead of unwrapping.
    s = setCaret(fromLatex("\\begin{pmatrix} a \\end{pmatrix}"), { path: [0, "r0c0"], offset: 0 });
    expect(deleteBackward(s).caret).toEqual({ path: [], offset: 0 });
    // Root row start: nothing.
    expect(deleteBackward(createState())).toEqual(createState());
  });

  it("mirrors the rules for Delete at the end of a slot", () => {
    let s = setCaret(fromLatex("\\frac{a}{b}"), { path: [0, "num"], offset: 1 });
    expect(deleteForward(s).caret).toEqual({ path: [0, "den"], offset: 0 });
    s = setCaret(fromLatex("x^{}"), { path: [0, "base"], offset: 1 });
    expect(src(deleteForward(s))).toBe("x");
    s = setCaret(fromLatex("\\hat{x} y"), { path: [0, "body"], offset: 1 });
    expect(src(deleteForward(s))).toBe("xy");
    s = setCaret(fromLatex("\\frac{a}{b}"), { path: [0, "den"], offset: 1 });
    expect(deleteForward(s).caret).toEqual({ path: [], offset: 1 });
  });

  it("deletes inside text and removes empty text", () => {
    let s = typeText(insertTemplate(createState(), "text"), "ab");
    s = deleteBackward(deleteBackward(s));
    expect(s.root.children).toEqual([{ kind: "text", text: "" }]);
    s = deleteBackward(s);
    expect(s.root.children).toEqual([]);
  });
});

describe("grids", () => {
  it("adds and removes matrix rows and columns", () => {
    let s = insertTemplate(createState(), "matrix", { rows: 2, cols: 2 });
    s = typeText(s, "a");
    s = editGrid(s, "addRowAfter");
    expect(s.caret).toEqual({ path: [0, "r1c0"], offset: 0 });
    let m = s.root.children[0] as Extract<MathNode, { kind: "matrix" }>;
    expect(m.rows.map((r) => r.length)).toEqual([2, 2, 2]);
    s = editGrid(s, "addColAfter");
    expect(s.caret).toEqual({ path: [0, "r1c1"], offset: 0 });
    m = s.root.children[0] as Extract<MathNode, { kind: "matrix" }>;
    expect(m.rows.map((r) => r.length)).toEqual([3, 3, 3]);
    s = editGrid(s, "addColBefore");
    expect(s.caret).toEqual({ path: [0, "r1c1"], offset: 0 });
    s = editGrid(s, "removeCol");
    s = editGrid(s, "removeCol");
    s = editGrid(s, "removeRow");
    m = s.root.children[0] as Extract<MathNode, { kind: "matrix" }>;
    expect(m.rows.map((r) => r.length)).toEqual([2, 2]);
    expect(src(s)).toBe("\\begin{pmatrix} a & \\\\ & \\end{pmatrix}");
    // Never below one row or column.
    let one = insertTemplate(createState(), "matrix", { rows: 1, cols: 1 });
    one = editGrid(editGrid(one, "removeRow"), "removeCol");
    expect((one.root.children[0] as Extract<MathNode, { kind: "matrix" }>).rows).toHaveLength(1);
  });

  it("edits cases rows (two fixed columns) and aligned columns", () => {
    let s = insertTemplate(createState(), "cases");
    s = typeText(s, "x");
    s = move(s, "nextSlot");
    expect(s.caret.path).toEqual([0, "r0c1"]);
    s = typeText(s, "x>0");
    s = editGrid(s, "addRowAfter");
    expect(s.caret.path).toEqual([0, "r1c0"]);
    expect(editGrid(s, "addColAfter")).toBe(s);
    const c = s.root.children[0] as Extract<MathNode, { kind: "cases" }>;
    expect(c.rows).toHaveLength(3);
    let a = insertTemplate(createState(), "aligned", { rows: 1, cols: 2 });
    a = editGrid(a, "addColAfter");
    expect((a.root.children[0] as Extract<MathNode, { kind: "aligned" }>).rows[0]).toHaveLength(3);
    // Outside a grid nothing happens.
    const plain = typeText(createState(), "x");
    expect(editGrid(plain, "addRowAfter")).toBe(plain);
    // From a nested slot inside a cell, the enclosing grid is edited.
    let nested = fromLatex("\\begin{pmatrix} \\frac{a}{b} \\end{pmatrix}");
    nested = setCaret(nested, { path: [0, "r0c0", 0, "num"], offset: 1 });
    nested = editGrid(nested, "addRowBefore");
    expect(nested.caret).toEqual({ path: [0, "r0c0"], offset: 0 });
  });
});

describe("helpers", () => {
  it("compares math ignoring the caret and absent optional fields", () => {
    const a = latex.parse("\\frac{a}{b}").row;
    const s = setCaret(createState(a), { path: [0, "num"], offset: 1 });
    expect(sameMath(s.root, latex.parse("\\frac{a}{b}").row)).toBe(true);
    expect(sameMath(a, latex.parse("\\dfrac{a}{b}").row)).toBe(false);
    expect(sameMath({ kind: "row", children: [{ kind: "symbol", id: "x", command: undefined }] }, { kind: "row", children: [{ kind: "symbol", id: "x" }] })).toBe(true);
  });

  it("produces KaTeX-ready preview LaTeX", () => {
    expect(previewLatex(insertTemplate(createState(), "frac").root)).toBe("\\frac{\\textcolor{gray}{\\square}}{\\textcolor{gray}{\\square}}");
    expect(previewLatex(latex.parse("E = mc^2 \\label{eq:a} \\nonumber").row)).toBe("E = mc^2");
    expect(previewLatex(latex.parse("\\R^n + a\\,b").row)).toBe("\\textcolor{gray}{\\text{\\textbackslash{}R}}^n + a\\,b");
    expect(previewLatex(typst.parse("x + #h(1em) + thin").row)).toBe("x + \\textcolor{gray}{\\text{\\#h(1em)}} + \\,");
    expect(previewLatex(typst.parse("sum_(k=1)^n k = n(n + 1) / 2").row)).toBe("\\sum_{k=1}^n k = \\frac{n\\left( n + 1 \\right)}{2}");
    expect(previewLatex(latex.parse("\\mathbb{R}").row)).toBe("\\mathbb{R}");
  });

  it("keeps the layout of rewritten equation content", () => {
    expect(withContentLayout("\n  E = mc^2\n", "E = mc^3")).toBe("\n  E = mc^3\n");
    expect(withContentLayout("\n  a &= b \\\\\n  c &= d\n", "a &= b \\\\\nc &= e")).toBe("\n  a &= b \\\\\n  c &= e\n");
    expect(withContentLayout(" x ", "y")).toBe(" y ");
    expect(withContentLayout("x", "y")).toBe("y");
    expect(withContentLayout("\r\n  x\r\n", "a\nb")).toBe("\r\n  a\r\n  b\r\n");
  });
});
