import { beforeEach, describe, expect, it } from "vitest";

import type { Diagnostic } from "../bindings/Diagnostic";
import { publishProblems, useProblems } from "./problems";

const diagnostic = (message: string): Diagnostic => ({ severity: "error", message, location: null, hints: [] });

describe("problems", () => {
  beforeEach(() => useProblems.setState({ sources: {} }));

  it("publishes and replaces a source", () => {
    publishProblems({ id: "lean", label: "Lean", diagnostics: [diagnostic("a")], reveal: () => {} });
    publishProblems({ id: "lean", label: "Lean", diagnostics: [diagnostic("b"), diagnostic("c")], reveal: () => {} });
    expect(useProblems.getState().sources.lean?.diagnostics.map((d) => d.message)).toEqual(["b", "c"]);
  });

  it("keeps other sources when one changes", () => {
    publishProblems({ id: "a", label: "A", diagnostics: [diagnostic("x")], reveal: () => {} });
    publishProblems({ id: "b", label: "B", diagnostics: [], reveal: () => {} });
    publishProblems({ id: "b", label: "B", diagnostics: [diagnostic("y")], reveal: () => {} });
    expect(useProblems.getState().sources.a?.diagnostics.map((d) => d.message)).toEqual(["x"]);
    expect(useProblems.getState().sources.b?.diagnostics.map((d) => d.message)).toEqual(["y"]);
  });
});
