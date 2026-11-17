import { describe, expect, it } from "vitest";
import { adapterFor, sameMath } from "./index";
import type { MathNode, MathRow } from "./types";

const nodeFs = "node:fs";
const { readFileSync } = (await import(/* @vite-ignore */ nodeFs)) as { readFileSync: (path: string, encoding: "utf8") => string };
const fixture = (p: string) => readFileSync(`tests/fixtures/${p}`, "utf8");

const latex = adapterFor("latex");
const contents = (text: string) => latex.equations(text).map((e) => text.slice(e.contentStart, e.contentEnd));
const styles = (text: string) => latex.equations(text).map((e) => `${e.style}${e.display ? "!" : ""}`);
const only = (src: string): MathNode => {
  const r = latex.parse(src).row;
  expect(r.children).toHaveLength(1);
  return r.children[0] as MathNode;
};
/** parse → serialize → parse gives the same tree, and the serialization is a fixed point. */
function roundTrip(src: string): string {
  const first = latex.parse(src);
  const out = latex.serialize(first.row);
  const second = latex.parse(out.source);
  expect(sameMath(first.row, second.row), `${src} -> ${out.source}`).toBe(true);
  expect(latex.serialize(second.row).source).toBe(out.source);
  return out.source;
}

describe("LaTeX equation locator", () => {
  it("finds the equations of latex-basic", () => {
    const text = fixture("latex-basic/main.tex");
    expect(contents(text)).toEqual(["a^2 + b^2 = c^2", "\\alpha + \\beta \\leq \\gamma", "\n  E = m c^2 + \\sum_{i=1}^{n} x_i\n  \\label{eq:energy}\n"]);
    expect(styles(text)).toEqual(["$", "$", "equation!"]);
    const results = fixture("latex-basic/sections/results.tex");
    expect(contents(results)).toEqual(["\n  f(x) = \\frac{1}{2} x^3 - 4x + 7\n"]);
    expect(styles(results)).toEqual(["\\[!"]);
  });

  it("finds the equations of latex-constructs and latex-unicode-math", () => {
    const text = fixture("latex-constructs/main.tex");
    expect(contents(text)).toEqual([
      "\\frac{a}{b} + \\sqrt{x^2+1} \\in \\R",
      "\\left( \\sum_{k=0}^{\\infty} \\frac{1}{k!} \\right) = e",
      "\n  f(x) &= \\int_0^1 g(t)\\,dt \\\\\n  \\nabla \\cdot \\mathbf{v} &= \\partial_t \\rho \\neq 0\n",
      "x \\text{ if } y > 0",
      "\\operatorname{tr}(A) = \\sin\\theta",
    ]);
    expect(styles(text)[2]).toBe("align!");
    const uni = fixture("latex-unicode-math/main.tex");
    expect(contents(uni)).toEqual([
      "a^2 + b_i \\leq \\alpha",
      "α + β ≤ γ",
      "\n  \\int_0^1 f(x)\\,dx = \\sum_{k=1}^{n} \\frac{1}{k^2} \\neq 0\n",
      "\\mathbf{v}",
      "\\mathbb{R}",
    ]);
  });

  it("ignores escaped dollars, comments, verbatim, and the preamble", () => {
    const text = [
      "\\documentclass{article}",
      "\\newcommand{\\half}{$\\frac12$}",
      "\\begin{document}",
      "Price \\$5 and \\$6. % a comment with $x$",
      "\\verb|$y$| and \\verb*+$z$+",
      "\\begin{verbatim}",
      "$not math$",
      "\\end{verbatim}",
      "\\begin{lstlisting}$no$\\end{lstlisting}",
      "Line\\\\$a$ then \\(b\\) and \\[c\\] and $$d$$.",
      "\\begin{align*}",
      "  x &= \\begin{aligned} y \\end{aligned}",
      "\\end{align*}",
      "\\begin{math}e\\end{math} \\begin{gather}f\\end{gather} \\begin{multline*}g\\end{multline*}",
      "\\begin{eqnarray}h\\end{eqnarray} \\begin{displaymath}i\\end{displaymath} \\begin{equation*}j\\end{equation*}",
      "Unterminated $ dollar",
      "\\end{document}",
      "$after$",
    ].join("\n");
    expect(contents(text)).toEqual(["a", "b", "c", "d", "\n  x &= \\begin{aligned} y \\end{aligned}\n", "e", "f", "g", "h", "i", "j"]);
    expect(styles(text)).toEqual(["$", "\\(", "\\[!", "$$!", "align*!", "math", "gather!", "multline*!", "eqnarray!", "displaymath!", "equation*!"]);
  });

  it("treats a file without \\begin{document} as body text", () => {
    expect(contents("A $x$ and % $y$\n$z$")).toEqual(["x", "z"]);
  });

  it("reports the context at an offset", () => {
    const text = "\\documentclass{article}\n\\begin{document}\nText $x+1$ here. % note\n\\verb|v| end\n\\end{document}\n";
    const at = (needle: string, delta = 0) => latex.contextAt(text, text.indexOf(needle) + delta);
    expect(at("documentclass").kind).toBe("unavailable");
    expect(latex.contextAt(text, 0)).toEqual({ kind: "unavailable", reason: "preamble" });
    expect(at("Text")).toEqual({ kind: "text" });
    const math = at("x+1", 1);
    expect(math.kind).toBe("math");
    if (math.kind === "math") expect(text.slice(math.equation.contentStart, math.equation.contentEnd)).toBe("x+1");
    expect(at("x+1", 0).kind).toBe("math");
    expect(at("x+1", 3).kind).toBe("math");
    expect(at("$x", 0)).toEqual({ kind: "text" });
    expect(at("% note", 2)).toEqual({ kind: "unavailable", reason: "comment" });
    expect(at("% note", 0)).toEqual({ kind: "text" });
    expect(at("|v|", 1)).toEqual({ kind: "unavailable", reason: "verbatim" });
    expect(at("end\n\\end{document}")).toEqual({ kind: "text" });
    expect(latex.contextAt(text, text.length)).toEqual({ kind: "unavailable", reason: "after \\end{document}" });
  });

  it("wraps new equations", () => {
    expect(latex.wrap("x^2", false)).toBe("$x^2$");
    expect(latex.wrap("x^2", true)).toBe("\\[\n  x^2\n\\]");
    expect(contents(`a ${latex.wrap("x", false)} b ${latex.wrap("y", true)}`)).toEqual(["x", "\n  y\n"]);
  });
});

describe("LaTeX parser", () => {
  it("builds the tree for common constructs", () => {
    expect(latex.parse("a^2 + b").row).toEqual<MathRow>({
      kind: "row",
      children: [
        { kind: "scripts", base: { kind: "row", children: [{ kind: "ident", text: "a" }] }, sub: null, sup: { kind: "row", children: [{ kind: "number", text: "2" }] } },
        { kind: "symbol", id: "op.plus" },
        { kind: "ident", text: "b" },
      ],
    });
    expect(only("\\frac{a}{b}")).toMatchObject({ kind: "frac", num: { children: [{ text: "a" }] }, den: { children: [{ text: "b" }] } });
    expect(only("\\frac12")).toMatchObject({ kind: "frac", num: { children: [{ text: "1" }] }, den: { children: [{ text: "2" }] } });
    expect(only("\\dfrac{a}{b}")).toMatchObject({ kind: "frac", variant: "dfrac" });
    expect(only("\\tfrac{a}{b}")).toMatchObject({ kind: "frac", variant: "tfrac" });
    expect(only("\\binom{n}{k}")).toMatchObject({ kind: "frac", variant: "binom" });
    expect(only("\\sqrt[3]{x}")).toMatchObject({ kind: "sqrt", index: { children: [{ text: "3" }] }, body: { children: [{ text: "x" }] } });
    expect(only("\\sum_{i=1}^{n}")).toMatchObject({ kind: "bigop", op: "bigop.sum", lower: { children: [{ text: "i" }, { id: "rel.eq" }, { text: "1" }] }, upper: { children: [{ text: "n" }] } });
    expect(only("\\int\\limits_0^1")).toMatchObject({ kind: "bigop", op: "bigop.int", limits: true });
    expect(only("\\oint")).toMatchObject({ kind: "bigop", op: "bigop.oint", lower: null, upper: null });
    expect(only("\\lim_{x\\to 0}")).toMatchObject({ kind: "bigop", op: "func.lim", lower: { children: [{ text: "x" }, { id: "arrow.r", command: "\\to" }, { text: "0" }] } });
    expect(only("\\sin")).toEqual({ kind: "func", name: "func.sin" });
    expect(latex.parse("\\sin^2 x").row.children[0]).toMatchObject({ kind: "scripts", base: { children: [{ kind: "func", name: "func.sin" }] } });
    expect(only("\\operatorname{tr}")).toEqual({ kind: "func", name: "func.tr" });
    expect(only("\\operatorname{Hom}")).toEqual({ kind: "func", name: "func.operatorname", text: "Hom" });
    expect(only("\\left( x \\right)")).toEqual({ kind: "delim", left: "delim.paren.l", right: "delim.paren.r", body: { kind: "row", children: [{ kind: "ident", text: "x" }] } });
    expect(only("\\left. x \\right|")).toMatchObject({ kind: "delim", left: "delim.none", right: "delim.vert" });
    expect(only("\\left\\langle x \\right\\rangle")).toMatchObject({ kind: "delim", left: "delim.angle.l", right: "delim.angle.r" });
    expect(only("(x)")).toMatchObject({ kind: "delim", left: "delim.paren.l", right: "delim.paren.r", form: "plain" });
    expect(only("\\{x\\}")).toMatchObject({ kind: "delim", left: "delim.brace.l", right: "delim.brace.r", form: "plain" });
    expect(only("\\lvert x \\rvert")).toMatchObject({ kind: "delim", left: "delim.vert.l", right: "delim.vert.r", form: "plain" });
    expect(latex.parse("[0, 1)").row.children.map((n) => (n.kind === "symbol" ? n.id : n.kind))).toEqual(["delim.bracket.l", "number", "punct.comma", "number", "delim.paren.r"]);
    expect(only("\\hat{x}")).toMatchObject({ kind: "accent", accent: "accent.hat" });
    expect(only("\\widehat{xy}")).toMatchObject({ kind: "accent", accent: "accent.widehat" });
    expect(only("\\underbrace{a+b}")).toMatchObject({ kind: "accent", accent: "accent.underbrace" });
    for (const [cmd, id] of [["mathbb", "font.bb"], ["mathcal", "font.cal"], ["mathbf", "font.bf"], ["mathrm", "font.rm"], ["mathfrak", "font.frak"], ["mathsf", "font.sf"], ["mathit", "font.it"], ["mathscr", "font.scr"], ["boldsymbol", "font.boldsymbol"]]) {
      expect(only(`\\${cmd}{x}`)).toMatchObject({ kind: "font", font: id });
    }
    expect(only("\\text{ if }")).toEqual({ kind: "text", text: " if " });
    expect(only("\\textrm{a\\&b}")).toEqual({ kind: "text", text: "a&b", command: "\\textrm" });
    expect(only("\\not\\in")).toEqual({ kind: "symbol", id: "set.notin", command: "\\not\\in" });
    expect(only("\\le")).toEqual({ kind: "symbol", id: "rel.leq", command: "\\le" });
    expect(only("\\leq")).toEqual({ kind: "symbol", id: "rel.leq" });
    expect(only("≤")).toEqual({ kind: "symbol", id: "rel.leq" });
    expect(only("α")).toEqual({ kind: "ident", text: "α" });
    expect(latex.parse("3.14").row.children.map((n) => n.kind)).toEqual(["number", "number", "number", "number"]);
    expect(only("{}^{14}")).toMatchObject({ kind: "scripts", base: { children: [] }, sup: { children: [{ text: "1" }, { text: "4" }] } });
    expect(only("{a+b}^2")).toMatchObject({ kind: "scripts", base: { children: [{ text: "a" }, { id: "op.plus" }, { text: "b" }] } });
    expect(only("x_i^2")).toMatchObject({ kind: "scripts", sub: { children: [{ text: "i" }] }, sup: { children: [{ text: "2" }] } });
    // A single-token argument does not pair delimiters: `x^(` is a superscript parenthesis.
    expect(latex.parse("x^(a)").row.children[0]).toMatchObject({ kind: "scripts", sup: { children: [{ id: "delim.paren.l" }] } });
  });

  it("parses matrices, cases, and alignments", () => {
    expect(only("\\begin{bmatrix} 1 & 2 \\\\ 3 & 4 \\end{bmatrix}")).toMatchObject({ kind: "matrix", fence: "delim.bracket", rows: [[{ children: [{ text: "1" }] }, { children: [{ text: "2" }] }], [{}, {}]] });
    for (const [env, fence] of [["matrix", "none"], ["pmatrix", "delim.paren"], ["Bmatrix", "delim.brace"], ["vmatrix", "delim.vert"], ["Vmatrix", "delim.Vert"]]) {
      expect(only(`\\begin{${env}} a \\end{${env}}`)).toMatchObject({ kind: "matrix", fence });
    }
    const cases = only("\\begin{cases} x & x > 0 \\\\ 0 & \\text{otherwise} \\end{cases}");
    expect(cases).toMatchObject({ kind: "cases", rows: [[{ children: [{ text: "x" }] }, { children: [{ text: "x" }, { id: "rel.gt" }, { text: "0" }] }], [{}, { children: [{ kind: "text", text: "otherwise" }] }]] });
    expect(only("\\begin{aligned} a &= b \\\\ c &= d \\end{aligned}")).toMatchObject({ kind: "aligned", rows: [[{}, {}], [{}, {}]] });
    expect(only("\\begin{gathered} a \\\\ b \\end{gathered}")).toMatchObject({ kind: "aligned", environment: "gathered" });
    // Rows of an align body.
    const body = only("\n  a &= b \\\\\n  c &= d \\\\\n");
    expect(body).toMatchObject({ kind: "aligned", bare: true });
    if (body.kind === "aligned") expect(body.rows).toHaveLength(2);
  });

  it("keeps unknown source as opaque nodes, byte for byte", () => {
    const src = "E = mc^2 \\label{eq:energy} \\tag*{A} \\nonumber + \\R + \\foo{a}[b]{c} + \\,x\\; + \\color{red}{y} + \\begin{array}{cc} 1 & 2 \\end{array} + 3{,}5";
    const parsed = latex.parse(src);
    expect(parsed.complete).toBe(false);
    const opaque = parsed.row.children.filter((n) => n.kind === "opaque").map((n) => (n as Extract<MathNode, { kind: "opaque" }>).source);
    expect(opaque).toEqual(["\\label{eq:energy}", "\\tag*{A}", "\\nonumber", "\\R", "\\foo{a}[b]{c}", "\\,", "\\;", "\\color{red}{y}", "\\begin{array}{cc} 1 & 2 \\end{array}", "{,}"]);
    const out = latex.serialize(parsed.row).source;
    for (const o of opaque) expect(out).toContain(o);
    roundTrip(src);
    expect(latex.parse("\\left( x").row.children[0]).toMatchObject({ kind: "opaque", source: "\\left( x" });
    // Unbalanced braces keep the whole content verbatim.
    expect(latex.parse("a } b").row.children).toEqual([{ kind: "opaque", source: "a } b", language: "latex", reason: "unbalanced braces" }]);
    expect(latex.serialize(latex.parse("\\frac{a").row).source).toBe("\\frac{a");
    expect(latex.parse("x % comment\n+ y").row.children[1]).toMatchObject({ kind: "opaque", source: "% comment" });
    expect(latex.serialize(latex.parse("x % comment\n+ y").row).source).toBe("x% comment\n+ y");
  });

  it("is complete for fully understood content", () => {
    expect(latex.parse("\\alpha + \\beta \\leq \\gamma").complete).toBe(true);
    expect(latex.parse("x \\label{a}").complete).toBe(false);
  });
});

describe("LaTeX serializer", () => {
  it("is byte-identical for canonical content", () => {
    const canonical = [
      "a^2 + b^2 = c^2",
      "\\alpha + \\beta \\leq \\gamma",
      "E = mc^2 + \\sum_{i=1}^n x_i",
      "f(x) = \\frac{1}{2} x^3 - 4x + 7",
      "\\frac{a}{b} + \\sqrt{x^2 + 1} \\in \\mathbb{R}",
      "\\left( \\sum_{k=0}^{\\infty} \\frac{1}{k!} \\right) = e",
      "x \\text{ if } y > 0",
      "\\operatorname{tr}(A) = \\sin \\theta",
      "\\lim_{x\\to 0} \\frac{\\sin x}{x} = 1",
      "\\int_0^1 f(x)\\,dx",
      "\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}",
      "f(x) = \\begin{cases} x & x > 0 \\\\ 0 & \\text{otherwise} \\end{cases}",
      "\\begin{aligned} a &= b \\\\ c &= d \\end{aligned}",
      "\\sqrt[3]{x} \\dfrac{1}{2} \\binom{n}{k}",
      "\\hat{x} + \\bar{y} + \\vec{v} + \\overline{z} + \\underbrace{a + b}_n",
      "\\not\\equiv \\le \\ne \\to",
      "[0, 1) \\cup (2, 3]",
      "-x + (-1)",
      "\\left\\langle u, v \\right\\rangle",
      "x' + f''(x)",
      "e^{i\\pi} + 1 = 0",
      "\\left. \\frac{df}{dx} \\right|_{x=0}",
      "\\mathcal{L} \\{f\\}(s)",
      "a \\bmod b",
      "\\sum\\limits_i a_i",
      "\\{x \\mid x > 0\\}",
    ];
    for (const src of canonical) expect(roundTrip(src), src).toBe(src);
  });

  it("round-trips every fixture equation semantically", () => {
    for (const file of ["latex-basic/main.tex", "latex-basic/sections/results.tex", "latex-constructs/main.tex", "latex-unicode-math/main.tex"]) {
      for (const c of contents(fixture(file))) roundTrip(c);
    }
  });

  it("round-trips tricky inputs semantically", () => {
    for (const src of [
      "\\frac12",
      "x^\\alpha_\\beta",
      "\\sqrt{\\frac{a}{b}}",
      "\\left[ \\frac{a}{b} \\right]^2",
      "{x^2}^3",
      "\\sum_{\\substack{i<j}} x",
      "\\mathbf{x}^T A \\mathbf{x}",
      "\\vec{v} \\cdot \\nabla",
      "a\\,b\\quad c",
      "\\displaystyle\\int f",
      "\\overset{!}{=}",
      "\\left( \\left[ x \\right] \\right)",
      "\\begin{pmatrix} \\begin{matrix} a \\end{matrix} & b \\end{pmatrix}",
      "\\text{a $b$ c}",
      "x_{\\text{max}}",
      "\\#\\$\\%\\&\\_",
      "\\big( x \\big)",
    ]) {
      roundTrip(src);
    }
  });

  it("reports package requirements", () => {
    const req = (src: string) => latex.serialize(latex.parse(src).row).requirements.map((r) => r.name);
    expect(req("a + b")).toEqual([]);
    expect(req("\\mathbb{R}")).toEqual(["amssymb"]);
    expect(req("\\mathscr{A}")).toEqual(["mathrsfs"]);
    expect(req("\\text{x} \\dfrac{1}{2}")).toEqual(["amsmath"]);
    expect(req("\\begin{pmatrix} a \\end{pmatrix} \\leqslant")).toEqual(["amsmath", "amssymb"]);
    expect(req("\\not\\leq")).toEqual([]);
    expect(req("\\nleq")).toEqual(["amssymb"]);
  });

  it("reports nodes from other languages as unsupported", () => {
    const row: MathRow = { kind: "row", children: [{ kind: "ident", text: "x" }, { kind: "opaque", source: "#h(1em)", language: "typst", reason: "code" }] };
    const out = latex.serialize(row);
    expect(out.source).toBe("x");
    expect(out.unsupported).toHaveLength(1);
  });
});

describe("LaTeX requirements in documents", () => {
  const doc = "\\documentclass{article}\n\\usepackage[utf8]{inputenc}\n\\usepackage{amsmath, mathtools}\n% \\usepackage{amssymb}\n\\begin{document}\nx\n\\end{document}\n";

  it("checks \\usepackage lines with option and comma lists", () => {
    const has = (name: string, text = doc) => latex.satisfied(text, { kind: "latexPackage", name });
    expect(has("amsmath")).toBe(true);
    expect(has("mathtools")).toBe(true);
    expect(has("inputenc")).toBe(true);
    expect(has("amssymb")).toBe(false);
    expect(has("amsmath", "\\usepackage{mathtools}")).toBe(true);
    expect(has("amssymb", "\\usepackage{unicode-math}")).toBe(true);
    expect(has("amssymb", "\\RequirePackage[x]{amssymb}")).toBe(true);
  });

  it("proposes a \\usepackage line after the last one", () => {
    const edit = latex.addRequirement(doc, { kind: "latexPackage", name: "amssymb" });
    expect(edit).not.toBeNull();
    if (!edit) return;
    const next = doc.slice(0, edit.offset) + edit.insert + doc.slice(edit.offset);
    expect(next).toContain("\\usepackage{amsmath, mathtools}\n\\usepackage{amssymb}\n% \\usepackage{amssymb}");
    const crlf = "\\documentclass{article}\r\n\\begin{document}\r\nx\r\n\\end{document}\r\n";
    const e2 = latex.addRequirement(crlf, { kind: "latexPackage", name: "amsmath" });
    expect(e2).toEqual({ offset: crlf.indexOf("\\begin"), insert: "\\usepackage{amsmath}\r\n" });
    expect(latex.addRequirement("Just a fragment $x$", { kind: "latexPackage", name: "amsmath" })).toBeNull();
    // Packages after \begin{document} are never proposed.
    const late = "\\documentclass{article}\n\\begin{document}\n\\usepackage{x}\n";
    expect(latex.addRequirement(late, { kind: "latexPackage", name: "y" })?.offset).toBe(late.indexOf("\\begin"));
  });
});
