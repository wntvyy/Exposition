import { describe, expect, it } from "vitest";
import { adapterFor, sameMath } from "./index";

const nodeFs = "node:fs";
const { readFileSync } = (await import(/* @vite-ignore */ nodeFs)) as { readFileSync: (path: string, encoding: "utf8") => string };

const md = adapterFor("markdown");
const contents = (text: string) => md.equations(text).map((e) => text.slice(e.contentStart, e.contentEnd));
const displays = (text: string) => md.equations(text).map((e) => e.display);

describe("Markdown equation locator (pulldown-cmark math rules)", () => {
  it("finds the equations of markdown-basic", () => {
    const text = readFileSync("tests/fixtures/markdown-basic/main.md", "utf8");
    expect(contents(text)).toEqual([
      "e^{i\\pi} + 1 = 0",
      "\\alpha^2 + \\beta_1 \\le \\sqrt{x}",
      "x_i",
      "\\pi",
      "e",
      "\n\\int_0^1 x^2 \\, dx = \\frac{1}{3}\n",
      "n!",
    ]);
    expect(displays(text)).toEqual([false, false, false, false, false, true, false]);
    expect(md.equations(text)[5]?.style).toBe("$$");
  });

  it("applies the whitespace, escape, and pairing rules", () => {
    expect(contents("a $ x$ b")).toEqual([]);
    expect(contents("a $x $ b")).toEqual([]);
    expect(contents("costs \\$5 and \\$6")).toEqual([]);
    expect(contents("costs $5 and $6")).toEqual([]);
    expect(contents("$a$ and $b$")).toEqual(["a", "b"]);
    expect(contents("$x$y")).toEqual(["x"]);
    expect(contents("$$x$$ inline display")).toEqual(["x"]);
    expect(displays("text $$x$$ more")).toEqual([true]);
    expect(contents("$a\\$b$")).toEqual(["a\\$b"]);
    // A single `$` inside `$$...$$` invalidates the display span, and neither inner `$` can open.
    expect(contents("$$ a $ b $$")).toEqual([]);
    expect(contents("$\\{x\\}$ and { $y$ }")).toEqual(["\\{x\\}", "y"]);
    expect(contents("$}$")).toEqual([]);
    expect(contents("$}$2+2$")).toEqual(["2+2"]);
    expect(contents("$a\nb$")).toEqual(["a\nb"]);
    expect(contents("$a\n\nb$")).toEqual([]);
  });

  it("ignores code spans, code blocks, front matter, and HTML comments", () => {
    const text = [
      "---",
      "title: $not$",
      "---",
      "",
      "Inline `$code$` and ``$`$`` then $m$.",
      "",
      "```",
      "$fenced$",
      "```",
      "",
      "    $indented$",
      "",
      "~~~md",
      "$tilde$",
      "~~~",
      "<!-- $comment$ -->",
      "- item $l$",
      "  - nested $k$",
      "> quote $q$",
      "# Heading $h$",
      "| $t$ | $u$ |",
      "|---|---|",
    ].join("\n");
    expect(contents(text)).toEqual(["m", "l", "k", "q", "h", "t", "u"]);
  });

  it("reports the context at an offset", () => {
    const text = "---\na: 1\n---\nText `co$de` and $x+1$ here.\n\n```\nblock\n```\n";
    const at = (needle: string, delta = 0) => md.contextAt(text, text.indexOf(needle) + delta);
    expect(md.contextAt(text, 0)).toEqual({ kind: "unavailable", reason: "front matter" });
    expect(at("a: 1")).toEqual({ kind: "unavailable", reason: "front matter" });
    expect(at("Text")).toEqual({ kind: "text" });
    expect(at("co$de")).toEqual({ kind: "unavailable", reason: "code" });
    expect(at("x+1", 1).kind).toBe("math");
    expect(at("block")).toEqual({ kind: "unavailable", reason: "code block" });
  });

  it("wraps new equations under the print profile rules", () => {
    expect(md.wrap(" x ", false)).toBe("$x$");
    expect(md.wrap("x", true)).toBe("$$\nx\n$$");
    expect(contents(`a ${md.wrap("y", false)} b`)).toEqual(["y"]);
  });
});

describe("Markdown math content", () => {
  it("parses the LaTeX subset and round-trips the fixture equations", () => {
    const text = readFileSync("tests/fixtures/markdown-basic/main.md", "utf8");
    for (const c of contents(text)) {
      const first = md.parse(c);
      // Only the `\,` spacing command is kept as opaque source.
      expect(first.complete, c).toBe(!c.includes("\\,"));
      const out = md.serialize(first.row);
      expect(out.unsupported, c).toEqual([]);
      expect(out.requirements).toEqual([]);
      expect(sameMath(md.parse(out.source).row, first.row), c).toBe(true);
    }
  });

  it("treats % as a character, not a comment", () => {
    expect(md.parse("50% x").row.children.map((n) => n.kind)).toEqual(["number", "number", "symbol", "ident"]);
  });

  it("reports what the print profile cannot render instead of emitting it silently", () => {
    const unsupported = (src: string) => md.serialize(md.parse(src).row).unsupported.map((u) => u.what);
    expect(unsupported("x \\label{eq:a}")).toEqual(["opaque"]);
    expect(unsupported("\\oiint_S f")).toEqual(["bigop.oiint"]);
    expect(unsupported("\\cancel{x}")).toEqual(["accent.cancel"]);
    expect(unsupported("a \\boxplus b")).toEqual(["op.boxplus"]);
    expect(unsupported("\\left\\llbracket x \\right\\rrbracket")).toEqual(["delim.dbracket.l", "delim.dbracket.r"]);
    expect(unsupported("\\begin{array}{c} x \\end{array}")).toEqual(["opaque"]);
    // Supported forms are accepted.
    expect(unsupported("a\\,b \\quad \\nonumber \\mathscr{A} \\binom{n}{k} \\left\\langle x \\right\\rangle \\operatorname{rank} A")).toEqual([]);
  });

  it("writes Markdown-accepted spellings", () => {
    const row = adapterFor("latex").parse("a \\nleq b \\nsubseteq c").row;
    const out = md.serialize(row);
    expect(out.source).toBe("a \\not\\leq b \\not\\subseteq c");
    expect(out.unsupported).toEqual([]);
    expect(out.requirements).toEqual([]);
    expect(adapterFor("latex").serialize(row).requirements).toEqual([{ kind: "latexPackage", name: "amssymb" }]);
  });

  it("has no package requirements", () => {
    expect(md.satisfied("", { kind: "latexPackage", name: "amsmath" })).toBe(true);
    expect(md.addRequirement("", { kind: "latexPackage", name: "amsmath" })).toBeNull();
  });
});
