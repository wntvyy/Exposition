import { beforeEach, describe, expect, it } from "vitest";

import type { Diagnostic } from "../bindings/Diagnostic";
import { clearProblems, publishProblems, useProblems } from "./problems";

const diagnostic = (message: string): Diagnostic => ({ severity: "error", message, location: null, hints: [] });

describe("problems", () => {
  beforeEach(() => useProblems.setState({ sources: {} }));

  it("publishes, replaces, and clears a source", () => {
    publishProblems({ id: "lean", label: "Lean", diagnostics: [diagnostic("a")], reveal: () => {} });
    publishProblems({ id: "lean", label: "Lean", diagnostics: [diagnostic("b"), diagnostic("c")], reveal: () => {} });
    expect(useProblems.getState().sources.lean?.diagnostics.map((d) => d.message)).toEqual(["b", "c"]);
    clearProblems("lean");
    expect(useProblems.getState().sources).toEqual({});
  });

  it("keeps other sources when one changes", () => {
    publishProblems({ id: "a", label: "A", diagnostics: [diagnostic("x")], reveal: () => {} });
    publishProblems({ id: "b", label: "B", diagnostics: [], reveal: () => {} });
    clearProblems("b");
    expect(Object.keys(useProblems.getState().sources)).toEqual(["a"]);
  });
});
