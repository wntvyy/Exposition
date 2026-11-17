import { describe, expect, it } from "vitest";
import { adapterFor, sameMath } from "./index";
import type { MathNode, MathRow } from "./types";

const nodeFs = "node:fs";
const { readFileSync } = (await import(/* @vite-ignore */ nodeFs)) as { readFileSync: (path: string, encoding: "utf8") => string };
const fixture = (p: string) => readFileSync(`tests/fixtures/${p}`, "utf8");

const typst = adapterFor("typst");
const contents = (text: string) => typst.equations(text).map((e) => text.slice(e.contentStart, e.contentEnd));
const only = (src: string): MathNode => {
  const r = typst.parse(src).row;
  expect(r.children, src).toHaveLength(1);
  return r.children[0] as MathNode;
};
function roundTrip(src: string): string {
  const first = typst.parse(src);
  const out = typst.serialize(first.row);
  const second = typst.parse(out.source);
  expect(sameMath(first.row, second.row), `${src} -> ${out.source}`).toBe(true);
  expect(typst.serialize(second.row).source).toBe(out.source);
  return out.source;
}

describe("Typst equation locator", () => {
  it("finds the fixture equations with inline/block styles", () => {
    const text = fixture("typst-basic/main.typ");
    expect(contents(text)).toEqual(["a^2 + b^2 = c^2", " sum_(k=1)^n k = (n (n + 1)) / 2 "]);
    expect(typst.equations(text).map((e) => [e.style, e.display])).toEqual([
      ["typst-inline", false],
      ["typst-block", true],
    ]);
    expect(contents(fixture("typst-unicode/main.typ"))).toEqual(["𝔸 + x^2"]);
  });

  it("ignores comments, raw text, escapes, and strings in code", () => {
    const text = [
      "// $a$ in a comment",
      "/* $b$ /* nested $c$ */ still */ text",
      "`$d$` and ```py $e$``` and \\$5 \\$6",
      '#let s = "$f$"',
      "#let g = $x + 1$",
      "#box[inline $y$ in content]",
      '#figure(rect(), caption: [A $z$ caption with "quotes"])',
      "Link https://example.com/$p$ stays", // `//` after `https:` is not a comment
      '$ "a $ b" $',
      "#{ let h = 1; [$w$] }",
      "Unterminated $ math",
    ].join("\n");
    expect(contents(text)).toEqual(["x + 1", "y", "z", "p", ' "a $ b" ', "w"]);
  });

  it("reports the context at an offset", () => {
    const text = '#set page(width: 10cm)\nText $x$ and #emph[more $y$] // note\n#let a = "s"\n';
    const at = (needle: string, delta = 0) => typst.contextAt(text, text.indexOf(needle) + delta);
    expect(at("page")).toEqual({ kind: "unavailable", reason: "code" });
    expect(at("Text")).toEqual({ kind: "text" });
    expect(at("x$").kind).toBe("math");
    expect(at("more")).toEqual({ kind: "text" });
    expect(at("y$").kind).toBe("math");
    expect(at("emph")).toEqual({ kind: "unavailable", reason: "code" });
    expect(at("note")).toEqual({ kind: "unavailable", reason: "comment" });
    expect(at('"s"', 1)).toEqual({ kind: "unavailable", reason: "string" });
  });

  it("wraps new equations", () => {
    expect(typst.wrap("x^2", false)).toBe("$x^2$");
    expect(typst.wrap("x^2", true)).toBe("$ x^2 $");
    const eq = typst.equations(`a ${typst.wrap("x", true)} b`)[0];
    expect(eq?.display).toBe(true);
  });

  it("has no package requirements", () => {
    expect(typst.satisfied("", { kind: "latexPackage", name: "amsmath" })).toBe(true);
    expect(typst.addRequirement("", { kind: "latexPackage", name: "amsmath" })).toBeNull();
  });
});

describe("Typst parser", () => {
  it("builds the tree for common constructs", () => {
    expect(only("frac(a, b)")).toMatchObject({ kind: "frac", num: { children: [{ text: "a" }] }, den: { children: [{ text: "b" }] } });
    expect(only("a / b")).toMatchObject({ kind: "frac", variant: "slash" });
    expect(only("(a + b) / 2")).toMatchObject({ kind: "frac", variant: "slash", num: { children: [{ text: "a" }, { id: "op.plus" }, { text: "b" }] } });
    expect(only("f(x) / 2")).toMatchObject({ kind: "frac", num: { children: [{ text: "f" }, { kind: "delim" }] } });
    expect(only("sin(x) / x")).toMatchObject({ kind: "frac", num: { children: [{ kind: "func", name: "func.sin" }, { kind: "delim" }] } });
    expect(only("12 / 34")).toMatchObject({ kind: "frac", num: { children: [{ text: "1" }, { text: "2" }] }, den: { children: [{ text: "3" }, { text: "4" }] } });
    expect(only("sqrt(x)")).toMatchObject({ kind: "sqrt", index: null });
    expect(only("root(3, x)")).toMatchObject({ kind: "sqrt", index: { children: [{ text: "3" }] } });
    expect(only("binom(n, k)")).toMatchObject({ kind: "frac", variant: "binom" });
    expect(only("x^(a+b)")).toMatchObject({ kind: "scripts", sup: { children: [{ text: "a" }, {}, { text: "b" }] } });
    expect(only("x_1^2")).toMatchObject({ kind: "scripts", sub: { children: [{ text: "1" }] }, sup: { children: [{ text: "2" }] } });
    expect(only("10^5")).toMatchObject({ kind: "scripts", base: { children: [{ text: "1" }, { text: "0" }] } });
    expect(only("sum_(k=1)^n")).toMatchObject({ kind: "bigop", op: "bigop.sum", lower: { children: [{}, { id: "rel.eq" }, {}] }, upper: { children: [{ text: "n" }] } });
    expect(only("integral_a^b")).toMatchObject({ kind: "bigop", op: "bigop.int" });
    expect(only("product")).toMatchObject({ kind: "bigop", op: "bigop.prod" });
    expect(only("lim_(x -> 0)")).toMatchObject({ kind: "bigop", op: "func.lim", lower: { children: [{}, { id: "arrow.r" }, {}] } });
    expect(only("lr(chevron.l x chevron.r)")).toMatchObject({ kind: "delim", left: "delim.angle.l", right: "delim.angle.r" });
    expect(only("lr((x))")).toMatchObject({ kind: "delim", left: "delim.paren.l", right: "delim.paren.r" });
    expect(only("(x)")).toMatchObject({ kind: "delim", left: "delim.paren.l", right: "delim.paren.r" });
    expect(only("[0, 1)")).toMatchObject({ kind: "delim", left: "delim.bracket.l", right: "delim.paren.r" });
    expect(only("abs(x)")).toMatchObject({ kind: "delim", left: "delim.vert", right: "delim.vert", form: "func" });
    expect(only("norm(x)")).toMatchObject({ kind: "delim", left: "delim.Vert", form: "func" });
    expect(only("floor(x)")).toMatchObject({ kind: "delim", left: "delim.floor.l", form: "func" });
    expect(only("mat(delim: \"[\", 1, 2; 3, 4)")).toMatchObject({ kind: "matrix", fence: "delim.bracket", rows: [[{}, {}], [{}, {}]] });
    expect(only("mat(delim: #none, a)")).toMatchObject({ kind: "matrix", fence: "none" });
    expect(only("mat(1, 2; 3, 4)")).toMatchObject({ kind: "matrix", fence: "delim.paren" });
    expect(only("vec(a, b)")).toMatchObject({ kind: "matrix", form: "vec", rows: [[{}], [{}]] });
    expect(only('cases(x & "if" x > 0, 0 & "otherwise")')).toMatchObject({ kind: "cases", rows: [[{ children: [{ text: "x" }] }, { children: [{ kind: "text", text: "if" }, {}, {}, {}] }], [{}, { children: [{ kind: "text" }] }]] });
    for (const [fn, id] of <[string, string][]>[["hat", "accent.hat"], ["tilde", "accent.tilde"], ["macron", "accent.bar"], ["overline", "accent.overline"], ["arrow", "accent.vec"], ["dot", "accent.dot"], ["dot.double", "accent.ddot"], ["underline", "accent.underline"], ["overbrace", "accent.overbrace"], ["underbrace", "accent.underbrace"]]) {
      expect(only(`${fn}(x)`)).toMatchObject({ kind: "accent", accent: id });
    }
    for (const [fn, id] of <[string, string][]>[["bb", "font.bb"], ["cal", "font.cal"], ["bold", "font.boldsymbol"], ["upright", "font.rm"], ["frak", "font.frak"], ["sans", "font.sf"], ["italic", "font.it"]]) {
      expect(only(`${fn}(A)`)).toMatchObject({ kind: "font", font: id });
    }
    expect(only("bold(upright(v))")).toMatchObject({ kind: "font", font: "font.bf" });
    expect(only('"text"')).toEqual({ kind: "text", text: "text" });
    expect(only("sin")).toEqual({ kind: "func", name: "func.sin" });
    expect(only('op("Hom")')).toEqual({ kind: "func", name: "func.operatorname", text: "Hom" });
    expect(only('op("sgn")')).toEqual({ kind: "func", name: "func.sgn" });
    expect(only("alpha")).toEqual({ kind: "symbol", id: "greek.alpha" });
    expect(only("arrow.r.long")).toEqual({ kind: "symbol", id: "arrow.r.long" });
    expect(only("lt.eq")).toEqual({ kind: "symbol", id: "rel.leq" });
    for (const [sh, id] of <[string, string][]>[["<=", "rel.leq"], [">=", "rel.geq"], ["!=", "rel.neq"], ["->", "arrow.r"], ["=>", "arrow.r.double"], ["...", "dots.h"], ["-", "op.minus"], ["<==>", "arrow.lr.double.long"], ["|->", "arrow.mapsto"]]) {
      expect(only(sh)).toEqual({ kind: "symbol", id });
    }
    expect(only("oo")).toEqual({ kind: "symbol", id: "calc.infty" });
    expect(only("dif")).toEqual({ kind: "symbol", id: "calc.d" });
    expect(typst.parse("a b").row.children).toEqual([{ kind: "ident", text: "a" }, { kind: "ident", text: "b" }]);
    expect(typst.parse("f'").row.children).toEqual([{ kind: "ident", text: "f" }, { kind: "symbol", id: "calc.prime" }]);
    expect(only("\\$")).toEqual({ kind: "symbol", id: "punct.dollar" });
  });

  it("parses alignment points and line breaks into rows", () => {
    const a = only("a &= b \\ c &= d");
    expect(a).toMatchObject({ kind: "aligned", bare: true });
    if (a.kind === "aligned") expect(a.rows.map((r) => r.length)).toEqual([2, 2]);
  });

  it("keeps code, comments, and unknown names as opaque source", () => {
    const parsed = typst.parse('#h(1em) x // note\n + foo(y) + thin + #emoji.face + upright("d") + attach(x, tl: 2)');
    expect(parsed.complete).toBe(false);
    const opaque = parsed.row.children.filter((n) => n.kind === "opaque").map((n) => (n as Extract<MathNode, { kind: "opaque" }>).source);
    expect(opaque).toEqual(["#h(1em)", "// note", "foo(y)", "thin", "#emoji.face", "attach(x, tl: 2)"]);
    expect(only("attach(a + b, t: 2)")).toMatchObject({ kind: "scripts", base: { children: [{}, {}, {}] }, sup: { children: [{ text: "2" }] }, sub: null });
    roundTrip('#h(1em) x // note\n + foo(y) + thin + #emoji.face');
    expect(typst.serialize(typst.parse("x // c").row).source).toBe("x// c\n");
  });
});

describe("Typst serializer", () => {
  it("is byte-identical for canonical content", () => {
    for (const src of [
      "a^2 + b^2 = c^2",
      // `n(n + 1)` is one operand of `/` in Typst (implicit call), so no extra parentheses.
      "sum_(k=1)^n k = n(n + 1) / 2",
      "frac(a, b) + sqrt(x) + root(3, x)",
      "integral_a^b f(x) dif x",
      "lim_(x->0) sin(x) / x = 1",
      'mat(delim: "[", 1, 2; 3, 4)',
      'cases(x & "if" x > 0, 0 & "otherwise")',
      "vec(a, b)",
      "hat(x) + tilde(y) + arrow(v) + dot.double(x)",
      "bb(R) + cal(A) + bold(x) + upright(d)",
      "alpha <= beta != gamma",
      "x in RR, A subset.eq B",
      "abs(x) + norm(v) + lr(chevron.l u\\, v chevron.r)",
      "a &= b \\\nc &= d",
      "x^(-1) + e^(i pi)",
      "f'(x)",
      "[0, 1) union (2, 3]",
      "f(x)^2 + (a + b)^2",
      "x_(i j) y^2",
      "binom(n, k) + mat(delim: #none, a)",
      'op("Hom")(A, B)',
      "2x + 3y",
    ]) {
      expect(roundTrip(src), src).toBe(src);
    }
  });

  it("round-trips the fixture equations semantically", () => {
    for (const c of contents(fixture("typst-basic/main.typ"))) roundTrip(c);
    for (const c of contents(fixture("typst-unicode/main.typ"))) roundTrip(c);
  });

  it("writes LaTeX-parsed trees as valid Typst", () => {
    const latex = adapterFor("latex");
    const cases: [string, string][] = [
      ["\\frac{a+b}{2}", "frac(a + b, 2)"],
      ["x^{10} y_{ij}", "x^10 y_(i j)"],
      ["\\sum_{i=1}^{n} x_i^2", "sum_(i=1)^n x_i^2"],
      ["\\sin^2 x", "sin^2 x"],
      ["\\mathbf{v} \\cdot \\vec{w}", "bold(upright(v)) dot.op arrow(w)"],
      ["\\int_0^1 f(x)\\,dx", "integral_0^1 f(x) thin d x"],
      ["\\left| x \\right| + \\left\\| y \\right\\|", "abs(x) + norm(y)"],
      ["\\{x \\mid x \\in \\mathbb{N}\\}", "{x divides x in bb(N)}"],
      ["\\text{if } a, b", '"if " a, b'],
      ["\\frac{(a)}{b}", "frac((a), b)"],
      ["\\alpha2 + x2", "alpha 2 + x 2"],
      ["\\binom{n}{k}", "binom(n, k)"],
      ["{a+b}^2", "attach(a + b, t: 2)"],
    ];
    for (const [src, expected] of cases) {
      const out = typst.serialize(latex.parse(src).row);
      expect(out.source, src).toBe(expected);
      expect(out.unsupported, src).toEqual([]);
    }
    const labelled = typst.serialize(latex.parse("x \\label{a}").row);
    expect(labelled.unsupported).toHaveLength(1);
  });

  it("escapes argument separators inside calls", () => {
    const row: MathRow = { kind: "row", children: [{ kind: "frac", num: { kind: "row", children: [{ kind: "ident", text: "a" }, { kind: "symbol", id: "punct.comma" }, { kind: "ident", text: "b" }] }, den: { kind: "row", children: [{ kind: "number", text: "2" }] } }] };
    const out = typst.serialize(row).source;
    expect(out).toBe("frac(a\\, b, 2)");
    expect(sameMath(typst.parse(out).row, row)).toBe(true);
  });
});
