import { describe, expect, it } from "vitest";

import type { BuildEvent } from "../../bindings/BuildEvent";
import type { Diagnostic } from "../../bindings/Diagnostic";
import { diagnosticMarks, marksOutdated } from "./markerData";

function build(format: "typst" | "latex", sources: Array<[string, number | null]>, diagnostics: Diagnostic[]): BuildEvent {
  return {
    result: {
      id: `${format}-1`,
      target: { format, engine: format === "typst" ? "typst" : "pdfLatex", entry: sources[0]![0] },
      snapshot: "s",
      sources: sources.map(([path, revision]) => ({ path, document: revision === null ? null : "doc-1", revision, contentHash: "h" })),
      configuration: null,
      status: diagnostics.some((d) => d.severity === "error") ? "failed" : "succeeded",
      diagnostics,
      pages: [],
      mapping: "none",
      exports: [],
      durationMs: 1,
    },
    timings: { snapshotMs: 0, compileMs: 0, mapMs: 0, totalMs: 0 },
    lastGood: null,
    refusal: null,
  } as unknown as BuildEvent;
}

const error = (path: string, start: number, end: number, message: string, hints: string[] = []): Diagnostic => ({
  severity: "error",
  message,
  location: { path, range: { start, end } },
  hints,
});

describe("diagnosticMarks", () => {
  it("marks the diagnostics of a build that compiled the view's revision", () => {
    const b = build("typst", [["main.typ", 4], ["chapter.typ", null]], [
      error("main.typ", 10, 16, "unknown variable: nosuch", ["did you mean `nothing`?"]),
      error("chapter.typ", 0, 3, "elsewhere"),
      { severity: "warning", message: "no location", location: null, hints: [] },
    ]);
    expect(diagnosticMarks([b], "main.typ", 4)).toEqual([
      { start: 10, end: 16, severity: "error", message: "unknown variable: nosuch\nhint: did you mean `nothing`?", source: "Typst" },
    ]);
  });

  it("keeps the marks shown while no build compiled the current revision", () => {
    const b = build("typst", [["main.typ", 4]], [error("main.typ", 1, 2, "old")]);
    expect(diagnosticMarks([b], "main.typ", 5)).toBeNull();
    expect(diagnosticMarks([b], "other.typ", 4)).toBeNull();
  });

  it("clears the marks when a build of the current revision reports none", () => {
    expect(diagnosticMarks([build("typst", [["main.typ", 6]], [])], "main.typ", 6)).toEqual([]);
  });

  it("combines entries that compile the same file, naming each format", () => {
    const typst = build("typst", [["shared.typ", 2]], [error("shared.typ", 0, 1, "a")]);
    const tex = build("latex", [["main.tex", 3], ["shared.typ", 2]], [error("shared.typ", 5, 3, "b")]);
    expect(diagnosticMarks([typst, tex], "shared.typ", 2)).toEqual([
      { start: 0, end: 1, severity: "error", message: "a", source: "Typst" },
      { start: 3, end: 5, severity: "error", message: "b", source: "LaTeX" },
    ]);
  });
});

describe("marker recomputation", () => {
  it("is skipped when neither the revision nor a latest build changed (build activity only)", () => {
    const typst = build("typst", [["main.typ", 4]], [error("main.typ", 0, 1, "a")]);
    const other = build("latex", [["main.tex", 2]], []);
    const applied = { path: "main.typ", revision: 4, builds: [typst, other] };
    // queued / building / idle: the store hands out a new list of the same results.
    expect(marksOutdated(applied, "main.typ", 4, [typst, other])).toBe(false);
  });

  it("is due for a first pass, a new result, another revision, or a renamed document", () => {
    const typst = build("typst", [["main.typ", 4]], []);
    const next = build("typst", [["main.typ", 4]], []);
    const applied = { path: "main.typ", revision: 4, builds: [typst] };
    expect(marksOutdated(undefined, "main.typ", 4, [typst])).toBe(true);
    expect(marksOutdated(applied, "main.typ", 4, [next])).toBe(true);
    expect(marksOutdated(applied, "main.typ", 4, [typst, next])).toBe(true);
    expect(marksOutdated(applied, "main.typ", 4, [])).toBe(true);
    expect(marksOutdated(applied, "main.typ", 5, [typst])).toBe(true);
    expect(marksOutdated(applied, "renamed.typ", 4, [typst])).toBe(true);
  });
});
