import { describe, expect, it } from "vitest";

import type { BuildEvent } from "../../bindings/BuildEvent";
import type { BuildResult } from "../../bindings/BuildResult";
import type { DocumentOutline } from "../../bindings/DocumentOutline";
import type { OutlineItem } from "../../bindings/OutlineItem";
import type { EntryBuildState } from "../typesetting/store";
import { acceptAnswer, headerOf, itemName, pageList, relocate, type OutlineTarget } from "./model";

function item(id: string, header: string, start: number, extra: Partial<OutlineItem> = {}): OutlineItem {
  const [syntax = "", title = ""] = id.split("|");
  return {
    id,
    kind: syntax.includes("heading") ? "heading" : "declaration",
    entityKind: syntax.includes("heading") ? null : "theorem",
    syntax,
    typeLabel: syntax.includes("heading") ? "Section" : "Theorem",
    title,
    label: null,
    level: syntax.includes("heading") ? 3 : 0,
    range: { start, end: start + header.length + 10 },
    selection: { start, end: start + header.length },
    header,
    children: [],
    ...extra,
  };
}

function outline(revision: number, items: OutlineItem[], path = "main.tex"): DocumentOutline {
  return { path, revision, unsaved: true, extractor: "latex/1", coverage: "complete", detail: null, items, itemCount: items.length };
}

const target: OutlineTarget = { documentId: "doc-1", path: "main.tex", root: "C:/p" };

describe("acceptAnswer", () => {
  const answer = outline(5, []);
  it("accepts the newest answer for the shown target", () => {
    expect(acceptAnswer(answer, { seq: 2, target }, { seq: 2, target, shown: outline(4, []) })).toBe(true);
  });
  it("drops overtaken requests", () => {
    expect(acceptAnswer(answer, { seq: 1, target }, { seq: 2, target, shown: null })).toBe(false);
  });
  it("drops answers for another document, path, or project", () => {
    expect(acceptAnswer(answer, { seq: 2, target }, { seq: 2, target: { ...target, documentId: "doc-2" }, shown: null })).toBe(false);
    expect(acceptAnswer(answer, { seq: 2, target }, { seq: 2, target: { ...target, root: "C:/other" }, shown: null })).toBe(false);
    expect(acceptAnswer(outline(5, [], "other.tex"), { seq: 2, target }, { seq: 2, target, shown: null })).toBe(false);
    expect(acceptAnswer(answer, { seq: 2, target }, { seq: 2, target: null, shown: null })).toBe(false);
  });
  it("never goes back to an older revision", () => {
    expect(acceptAnswer(outline(3, []), { seq: 2, target }, { seq: 2, target, shown: outline(4, []) })).toBe(false);
  });
});

describe("relocate", () => {
  it("finds the same item after an edit elsewhere", () => {
    const thm = item("latex.env:theorem|Main|0", "\\begin{theorem}[Main]", 40);
    const before = outline(1, [thm]);
    const moved = item("latex.env:theorem|Main|0", "\\begin{theorem}[Main]", 52);
    expect(relocate(thm, before, outline(2, [moved]))).toBe(moved);
  });

  it("refuses a removed item instead of the next one written the same way", () => {
    const first = item("latex.env:theorem||0", "\\begin{theorem}", 10);
    const second = item("latex.env:theorem||1", "\\begin{theorem}", 80);
    const before = outline(1, [first, second]);
    // The first theorem was deleted: the old second one is now occurrence 0.
    const after = outline(2, [item("latex.env:theorem||0", "\\begin{theorem}", 10)]);
    expect(relocate(first, before, after)).toBeNull();
    expect(relocate(second, before, after)).toBeNull();
  });

  it("refuses an item whose first line changed or that is gone", () => {
    const thm = item("latex.env:theorem|Main|0", "\\begin{theorem}[Main]\\label{a}", 40);
    const before = outline(1, [thm]);
    expect(relocate(thm, before, outline(2, [item("latex.env:theorem|Main|0", "\\begin{theorem}[Main]\\label{b}", 40)]))).toBeNull();
    expect(relocate(thm, before, outline(2, []))).toBeNull();
    expect(relocate(thm, before, outline(2, [thm], "other.tex"))).toBeNull();
  });

  it("looks into nested items", () => {
    const thm = item("latex.env:theorem|Main|0", "\\begin{theorem}[Main]", 40);
    const section = item("latex.heading:section|Intro|0", "\\section{Intro}", 0, { children: [thm] });
    expect(relocate(thm, outline(1, [section]), outline(2, [section]))?.id).toBe(thm.id);
  });
});

describe("display", () => {
  it("names rows readably", () => {
    expect(itemName(item("latex.env:theorem|Main result|0", "x", 0))).toBe("Theorem (Main result)");
    expect(itemName(item("latex.env:theorem||0", "x", 0))).toBe("Theorem");
    expect(itemName(item("latex.heading:section|Intro|0", "x", 0))).toBe("Intro");
    expect(itemName(item("latex.heading:section||0", "x", 0))).toBe("Section (untitled)");
  });
  it("normalizes headers like the backend", () => {
    expect(headerOf("  \\begin{theorem}[A   B]  \t")).toBe("\\begin{theorem}[A B]");
    expect(headerOf("x".repeat(300))).toHaveLength(200);
  });
});

function build(id: string, status: BuildResult["status"], revision: number, pages: number): BuildResult {
  return {
    id,
    target: { entry: "main.typ" } as BuildResult["target"],
    snapshot: "s" as BuildResult["snapshot"],
    sources: [{ path: "main.typ", document: "doc-1", revision, contentHash: "h" }],
    configuration: null,
    status,
    diagnostics: [],
    pages: Array.from({ length: pages }, (_, index) => ({ index, widthPt: 595, heightPt: 842, renderRef: `${id}-${index}` })),
    mapping: "exact" as BuildResult["mapping"],
    exports: [],
    durationMs: 1,
  };
}

function entry(latest: BuildResult, lastGood: BuildResult | null = null, activity: EntryBuildState["activity"] = "idle"): EntryBuildState {
  const event = { result: latest, lastGood, refusal: null, timings: {} } as unknown as BuildEvent;
  return { entry: "main.typ", activity, latest: event };
}

describe("pageList", () => {
  it("lists the pages of the current successful build", () => {
    const list = pageList(entry(build("b2", { kind: "succeeded" }, 4, 3)), "doc-1", 4);
    expect(list).toMatchObject({ state: "current", buildRevision: 4, pages: [{ index: 0 }, { index: 1 }, { index: 2 }] });
    expect(list.build?.id).toBe("b2");
  });

  it("says when the source changed since that build", () => {
    expect(pageList(entry(build("b2", { kind: "succeeded" }, 4, 1)), "doc-1", 6).state).toBe("earlier");
  });

  it("keeps the last good build's pages, labelled, after a failed build", () => {
    const list = pageList(entry(build("b3", { kind: "failed" }, 7, 0), build("b2", { kind: "succeeded" }, 4, 2)), "doc-1", 7);
    expect(list).toMatchObject({ state: "failed", buildRevision: 4 });
    expect(list.build?.id).toBe("b2");
    expect(list.pages).toHaveLength(2);
  });

  it("lists nothing without a successful build", () => {
    expect(pageList(undefined, "doc-1", 1).state).toBe("none");
    expect(pageList(entry(build("b1", { kind: "failed" }, 1, 0)), "doc-1", 1)).toMatchObject({ state: "none", pages: [] });
    expect(pageList(entry(build("b1", { kind: "succeeded" }, 1, 1), null, "building"), "doc-1", 1).building).toBe(true);
  });
});
