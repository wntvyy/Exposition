// Regression tests for the independent review of the equation core.
import { describe, expect, it } from "vitest";
import { adapterFor, catalog, createState, insertSymbol, insertTemplate, move, searchCatalog, typeText, withContentLayout, type MathNode } from "./index";

const latex = adapterFor("latex");
const typst = adapterFor("typst");
const contents = (a: ReturnType<typeof adapterFor>, text: string) => a.equations(text).map((e) => text.slice(e.start, e.end));

describe("LaTeX locator", () => {
  it("does not end an equation at a `$` nested in a brace group (\\text, \\mbox)", () => {
    expect(contents(latex, "A $\\text{if $x>0$}+z$ B")).toEqual(["$\\text{if $x>0$}+z$"]);
    expect(contents(latex, "\\[ a \\mbox{ for $b$ } \\] and $c$")).toEqual(["\\[ a \\mbox{ for $b$ } \\]", "$c$"]);
    expect(latex.parse("\\text{if $x>0$}+z").row.children[0]).toMatchObject({ kind: "opaque", source: "\\text{if $x>0$}" });
  });

  it("knows alignat-style environments; the column count stays outside the content", () => {
    const text = "\\begin{alignat}{2}\n  a &= b\n\\end{alignat} \\begin{xalignat*}{1} c \\end{xalignat*} \\begin{alignat*}{3}d\\end{alignat*}";
    const eqs = latex.equations(text);
    expect(eqs.map((e) => [e.style, text.slice(e.contentStart, e.contentEnd)])).toEqual([
      ["alignat", "\n  a &= b\n"],
      ["xalignat*", " c "],
      ["alignat*", "d"],
    ]);
    expect(latex.contextAt(text, text.indexOf("b\n")).kind).toBe("math");
  });

  it("reads \\url and \\href arguments verbatim (`%` there is not a comment)", () => {
    expect(contents(latex, "\\url{a%b} $w$")).toEqual(["$w$"]);
    expect(contents(latex, "\\href{http://x.y/%20}{link $v$} $w$")).toEqual(["$v$", "$w$"]);
  });
});

describe("LaTeX row breaks with modifiers", () => {
  it("keeps `\\\\[len]` and `\\\\*` attached to the row break, byte for byte", () => {
    for (const src of ["a \\\\[2pt]\nb", "\\begin{aligned} a &= 1 \\\\[1ex] b &= 2 \\end{aligned}", "a \\\\*\nb", "\\begin{cases} 1 & x \\\\[2pt] 2 & y \\end{cases}", "\\begin{pmatrix} 1 \\\\*[3pt] 2 \\end{pmatrix}"]) {
      expect(latex.serialize(latex.parse(src).row).source, src).toBe(src);
    }
    // After an edit elsewhere the modifier still follows `\\` directly.
    const edited = typeText(move(createState(latex.parse("a \\\\[2pt] b").row), "home"), "z");
    expect(latex.serialize(edited.root).source).toBe("za \\\\[2pt]\nb");
    // `\\ [a, b]` (with a space) is an interval on the next row, not a modifier.
    expect(latex.parse("a \\\\ [0, 1]").row.children[0]).toMatchObject({ kind: "aligned", rows: [[{}], [{ children: [{ kind: "delim", form: "plain" }] }]] });
  });
});

describe("comments at the end of display content", () => {
  it("never produces a white-space-only line", () => {
    const old = "\n  a = b % note\n";
    const out = latex.serialize(latex.parse(old).row).source;
    expect(out).toBe("a = b% note\n");
    expect(withContentLayout(old, out)).toBe("\n  a = b% note\n");
    expect(withContentLayout("\n  a\n", "b % c\nd")).toBe("\n  b % c\n  d\n");
    // Inline content gets the line end the comment needs before the closing `$`.
    expect(withContentLayout("x", "y % c\n")).toBe("y % c\n");
    expect(withContentLayout("", "x")).toBe("x");
  });
});

describe("LaTeX requirements", () => {
  it("inserts \\usepackage in the preamble even when \\begin{document} shares its line", () => {
    const text = "\\documentclass{article}\\begin{document}\nA $x$\n\\end{document}\n";
    const edit = latex.addRequirement(text, { kind: "latexPackage", name: "amssymb" });
    expect(edit).not.toBeNull();
    if (!edit) return;
    const next = text.slice(0, edit.offset) + edit.insert + text.slice(edit.offset);
    expect(next).toBe("\\documentclass{article}\n\\usepackage{amssymb}\n\\begin{document}\nA $x$\n\\end{document}\n");
    expect(latex.satisfied(next, { kind: "latexPackage", name: "amssymb" })).toBe(true);
  });
});

describe("Typst strings", () => {
  it("keeps escapes byte for byte and decodes them in the tree", () => {
    for (const src of ['"a\\nb"', '"\\u{3b1}"', '"tab\\tx"', '"x\\$"', '"a\\\\b"', '"q\\"q"']) {
      expect(typst.serialize(typst.parse(src).row).source, src).toBe(src);
    }
    expect(typst.parse('"a\\nb"').row.children[0]).toMatchObject({ kind: "text", text: "a\nb" });
    expect(typst.parse('"\\u{3b1}"').row.children[0]).toMatchObject({ kind: "text", text: "α" });
    // Edited text is written with the writer's escapes.
    expect(typst.serialize({ kind: "row", children: [{ kind: "text", text: "a\nb", raw: "stale" }] }).source).toBe('"a\\nb"');
  });
});

describe("Typst accents, names, and placeholders", () => {
  it("reads every accent form the writer emits", () => {
    const only = (src: string) => typst.parse(src).row.children[0] as MathNode;
    expect(only("arrow.l(x)")).toMatchObject({ kind: "accent", accent: "accent.overleftarrow" });
    expect(only("arrow.l.r(x)")).toMatchObject({ kind: "accent", accent: "accent.overleftrightarrow" });
    // Typst calls a symbol whose character is an accent as that accent.
    expect(only("arrow.r(x)")).toMatchObject({ kind: "accent", accent: "accent.vec" });
    expect(only("dot.op(x)")).toMatchObject({ kind: "accent", accent: "accent.dot" });
    expect(only("harpoon.rt(x)")).toMatchObject({ kind: "opaque" });
    for (const id of ["accent.overleftarrow", "accent.overleftrightarrow"]) {
      const tree = insertSymbol(createState(), id).root;
      expect(typst.parse(typst.serialize(tree).source).row).toEqual(tree);
    }
  });

  it("reads symbol names without modifiers as their first variant", () => {
    const id = (src: string) => (typst.parse(src).row.children[0] as Extract<MathNode, { kind: "symbol" }>).id;
    expect(id("dot")).toBe("op.cdot");
    expect(id("dots")).toBe("dots.c");
    expect(id("nabla")).toBe("calc.nabla");
    expect(id("harpoon")).toBe("harpoon.rt");
    expect(id("arrow")).toBe("arrow.r");
  });

  it("writes empty slots as the empty string", () => {
    const at = (t: Parameters<typeof insertTemplate>[1]) => typst.serialize(typeText(insertTemplate(typeText(createState(), "x"), t), "y").root).source;
    expect(at("frac")).toBe('x frac(y, "")');
    expect(at("root")).toBe('x root(y, "")');
    expect(at("sup")).toBe('x y^""');
    expect(at("integral")).toBe('x integral_y^""');
    expect(typst.parse('frac(y, "")').row).toEqual(typst.parse("frac(y, )").row);
  });

  it("reports entries without a verified Typst spelling instead of writing them silently", () => {
    const missing = catalog().filter((e) => !e.typst && e.role === "symbol");
    expect(missing.length).toBeGreaterThan(10);
    for (const e of missing) {
      const out = typst.serialize(insertSymbol(createState(), e.id).root);
      expect(out.unsupported.map((u) => u.what), e.id).toEqual([e.id]);
    }
  });

  it("reads the catalog's own operator calls", () => {
    expect(typst.parse('op("arg max", limits: #true)_x f').row.children[0]).toMatchObject({ kind: "bigop", op: "func.argmax" });
    expect(typst.parse('op("x", limits: #false)').row.children[0]).toMatchObject({ kind: "opaque" });
  });

  it("reports only the outermost of nested equations", () => {
    expect(contents(typst, "$x #[$y$] z$ and $w$")).toEqual(["$x #[$y$] z$", "$w$"]);
  });
});

describe("catalog symbols written with a style command", () => {
  it("read back as the symbol", () => {
    expect(latex.parse("\\mathbb{R}").row.children).toEqual([{ kind: "symbol", id: "set.bb.R" }]);
    expect(latex.parse("\\mathrm{d}x").row.children[0]).toEqual({ kind: "symbol", id: "calc.d" });
    expect(latex.parse("\\mathbb{x}").row.children[0]).toMatchObject({ kind: "font", font: "font.bb" });
    expect(latex.parse("\\stackrel{\\text{def}}{=}").row.children).toEqual([{ kind: "symbol", id: "rel.eqdef" }]);
    expect(typst.parse("upright(e)").row.children).toEqual([{ kind: "symbol", id: "letter.e" }]);
    expect(typst.parse("bb(k)").row.children).toEqual([{ kind: "symbol", id: "letter.Bbbk" }]);
  });
});

describe("search ranking", () => {
  const top = (q: string, n = 1) => searchCatalog(q, {}).slice(0, n).map((h) => h.entry.id);
  it("puts exact names, aliases, and glyphs first", () => {
    expect(top("x")).toEqual(["op.times"]);
    expect(top("R")).toEqual(["set.bb.R"]);
    expect(top("N")).toEqual(["set.bb.N"]);
    expect(top("dot", 5)).toContain("op.cdot");
    expect(top("dot")).toEqual(["op.cdot"]);
    expect(top("ℝ")).toEqual(["set.bb.R"]);
  });
});
