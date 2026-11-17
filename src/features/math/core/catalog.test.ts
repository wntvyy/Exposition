import { describe, expect, it } from "vitest";
import { catalog, catalogEntry, searchCatalog } from "./index";

const nodeFs = "node:fs";
const { readFileSync } = (await import(/* @vite-ignore */ nodeFs)) as { readFileSync: (path: string, encoding: "utf8") => string };

describe("catalog data", () => {
  const entries = catalog();

  it("is substantial and categorized", () => {
    expect(entries.length).toBeGreaterThanOrEqual(400);
    const byCategory = new Map<string, number>();
    for (const e of entries) byCategory.set(e.category, (byCategory.get(e.category) ?? 0) + 1);
    for (const c of ["Greek", "Binary operators", "Relations", "Negated relations", "Arrows", "Sets", "Logic", "Calculus", "Large operators", "Delimiters", "Functions", "Accents", "Alphabets", "Dots", "Letterlike"]) {
      expect(byCategory.get(c), c).toBeGreaterThan(0);
    }
    expect(byCategory.get("Greek")).toBeGreaterThanOrEqual(40);
    expect(byCategory.get("Arrows")).toBeGreaterThanOrEqual(50);
  });

  it("has unique ids and unique LaTeX spellings", () => {
    const ids = new Set<string>();
    const spellings = new Map<string, string>();
    for (const e of entries) {
      expect(ids.has(e.id), e.id).toBe(false);
      ids.add(e.id);
      if (!e.latex || e.id === "delim.none") continue;
      for (const s of [e.latex.command, ...(e.latex.alternatives ?? [])]) {
        expect(spellings.get(s), `${s} in ${e.id}`).toBeUndefined();
        spellings.set(s, e.id);
      }
    }
  });

  it("treats variants and different meanings as distinct entries", () => {
    expect(catalogEntry("greek.epsilon")?.glyph).toBe("ϵ");
    expect(catalogEntry("greek.varepsilon")?.glyph).toBe("ε");
    // Typst's `epsilon` is the curly form, LaTeX's \epsilon the lunate one.
    expect(catalogEntry("greek.epsilon")?.typst?.name).toBe("epsilon.alt");
    expect(catalogEntry("greek.varepsilon")?.typst?.name).toBe("epsilon");
    expect(catalogEntry("greek.phi")?.typst?.name).toBe("phi.alt");
    expect(catalogEntry("greek.varphi")?.typst?.name).toBe("phi");
    expect(catalogEntry("op.setminus")?.latex?.command).toBe("\\setminus");
    expect(catalogEntry("delim.backslash")?.latex?.command).toBe("\\backslash");
    expect(catalogEntry("rel.mid")?.latex?.command).toBe("\\mid");
    expect(catalogEntry("delim.vert")?.latex?.command).toBe("|");
    expect(catalogEntry("rel.leq")?.latex).toEqual({ command: "\\leq", alternatives: ["\\le"] });
    expect(catalogEntry("rel.leq")?.aliases).toEqual(expect.arrayContaining(["leq", "le", "<=", "less or equal"]));
  });

  it("names packages for commands outside base LaTeX", () => {
    expect(catalogEntry("font.bb")?.latex?.package).toBe("amssymb");
    expect(catalogEntry("font.scr")?.latex?.package).toBe("mathrsfs");
    expect(catalogEntry("bigop.iint")?.latex?.package).toBe("amsmath");
    expect(catalogEntry("rel.leqslant")?.latex?.package).toBe("amssymb");
    expect(catalogEntry("greek.alpha")?.latex?.package).toBeUndefined();
  });

  it("marks Markdown support exactly as the print profile's translator tables (crates/exposition-markdown/src/math.rs)", () => {
    const rs = readFileSync("crates/exposition-markdown/src/math.rs", "utf8");
    const table = (name: string) => {
      const start = rs.indexOf(`const ${name}:`);
      return rs.slice(start, rs.indexOf("];", start));
    };
    const firsts = (src: string) => [...src.matchAll(/\(\s*"((?:[^"\\]|\\.)*)"/g)].map((m) => (m[1] as string).replace(/\\\\/g, "\\"));
    const words = new Set([...firsts(table("SYMBOLS")), ...[...table("OPERATORS").matchAll(/"([^"]+)"/g)].map((m) => m[1] as string), ...firsts(table("UNARY")), ...firsts(table("BINARY")), "text", "textrm", "textit", "textbf", "mbox", "operatorname", "bmod", "sqrt"]);
    const unary = new Set(firsts(table("UNARY")));
    const negations = new Set(firsts(table("NEGATIONS")));
    const chars = new Set([..."+-*/=<>()[]|,;:.!'?"]);
    const supported = (cmd: string): boolean => {
      if (cmd.length === 1) return chars.has(cmd) || (!/[#$%&\\^_{}~]/.test(cmd) && cmd.charCodeAt(0) > 127);
      if (["\\{", "\\}", "\\|", "\\#", "\\$", "\\%", "\\&", "\\_"].includes(cmd)) return true;
      const not = /^\\not\s*(\\[A-Za-z]+|.)$/.exec(cmd);
      if (not) return negations.has((not[1] as string).replace(/^\\/, ""));
      const m = /^\\([A-Za-z]+)(.*)$/.exec(cmd);
      if (!m) return false;
      if (m[2] === "") return words.has(m[1] as string);
      if (/^\{[A-Za-z0-9]+\}$/.test(m[2] as string)) return unary.has(m[1] as string) || m[1] === "operatorname";
      return false;
    };
    const mismatches: string[] = [];
    for (const e of entries) {
      if (!e.latex) continue;
      const spellings = [e.latex.command, ...(e.latex.alternatives ?? [])];
      const expected = spellings.some(supported);
      if (expected !== e.markdown) mismatches.push(`${e.id}: flag ${e.markdown}, translator ${expected}`);
      if (e.latex.markdown !== undefined) {
        expect(supported(e.latex.markdown), e.id).toBe(true);
        expect(supported(e.latex.command), e.id).toBe(false);
      }
    }
    expect(mismatches).toEqual([]);
    expect(entries.filter((e) => e.markdown).length).toBeGreaterThan(250);
  });

  it("gives Typst names for nearly everything", () => {
    const missing = entries.filter((e) => !e.typst).map((e) => e.id);
    expect(missing.length).toBeLessThan(30);
    expect(catalogEntry("rel.leq")?.typst?.name).toBe("lt.eq");
    expect(catalogEntry("arrow.r.double")?.typst?.name).toBe("arrow.r.double");
    expect(catalogEntry("set.bb.R")?.typst?.name).toBe("RR");
    expect(catalogEntry("bigop.int")?.typst?.name).toBe("integral");
  });
});

describe("searchCatalog", () => {
  const first = (q: string) => searchCatalog(q, {})[0]?.entry.id;

  it("puts ≤ first for its name, command, and glyph", () => {
    expect(first("less than or equal")).toBe("rel.leq");
    expect(first("leq")).toBe("rel.leq");
    expect(first("≤")).toBe("rel.leq");
    expect(first("\\leq")).toBe("rel.leq");
    expect(first("le")).toBe("rel.leq");
    expect(first("<=")).toBe("rel.leq");
    expect(first("lt.eq")).toBe("rel.leq");
  });

  it("is case-insensitive but prefers the exact case", () => {
    expect(first("alpha")).toBe("greek.alpha");
    expect(first("ALPHA")).toBe("greek.alpha");
    expect(first("Delta")).toBe("greek.Delta");
    expect(first("delta")).toBe("greek.delta");
    expect(first("\\Rightarrow")).toBe("arrow.r.double");
  });

  it("ranks exact > prefix > word prefix > substring, ties by catalog order", () => {
    const hits = searchCatalog("int", {}).map((h) => h.entry.id);
    expect(hits[0]).toBe("bigop.int");
    // `integral` prefix matches come before substring matches like `point`.
    const iint = hits.indexOf("bigop.iint");
    const integralPrefix = hits.indexOf("bigop.oint");
    expect(iint).toBeGreaterThan(0);
    expect(integralPrefix).toBeGreaterThan(0);
    const order = searchCatalog("arrow", {}).map((h) => h.entry.id);
    expect(order.indexOf("arrow.r")).toBeLessThan(order.indexOf("arrow.hook.r"));
    expect(first("union")).toBe("op.cup");
    expect(first("sin")).toBe("func.sin");
    expect(first("for all")).toBe("logic.forall");
    expect(first("ℝ")).toBe("set.bb.R");
    expect(first("real numbers")).toBe("set.bb.R");
  });

  it("is deterministic and explains matches", () => {
    expect(searchCatalog("sub", {})).toEqual(searchCatalog("sub", {}));
    const hit = searchCatalog("≤", {})[0];
    expect(hit?.matched).toBe("glyph ≤");
    expect(searchCatalog("leq", {})[0]?.matched).toBe("alias leq");
  });

  it("filters by category, limits, and reports availability per language", () => {
    const greek = searchCatalog("", { category: "Greek", limit: 100 });
    expect(greek.length).toBe(catalog().filter((e) => e.category === "Greek").length);
    expect(greek.every((h) => h.entry.category === "Greek")).toBe(true);
    expect(searchCatalog("a", { limit: 5 })).toHaveLength(5);
    expect(searchCatalog("circeq", { language: "typst" })[0]).toMatchObject({ available: false });
    expect(searchCatalog("circeq", { language: "latex" })[0]).toMatchObject({ available: true });
    expect(searchCatalog("nleq", { language: "markdown" })[0]).toMatchObject({ available: true });
    expect(searchCatalog("oiint", { language: "markdown" })[0]).toMatchObject({ available: false });
  });

  it("is fast enough for every keystroke", () => {
    const queries = ["a", "al", "alp", "alph", "alpha", "l", "le", "leq", "arrow", "right arrow", "≤", "\\sum", "int", "x", "set", "not", "blackboard r"];
    // Warm up.
    for (const q of queries) searchCatalog(q, { language: "latex" });
    const rounds = 200;
    const start = performance.now();
    for (let i = 0; i < rounds; i++) for (const q of queries) searchCatalog(q, { language: "latex" });
    const perQuery = (performance.now() - start) / (rounds * queries.length);
    console.info(`searchCatalog: ${perQuery.toFixed(3)} ms per query over ${catalog().length} entries`);
    expect(perQuery).toBeLessThan(5);
  });
});
