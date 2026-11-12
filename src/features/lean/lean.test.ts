import { describe, expect, it } from "vitest";

import type { LeanStatus } from "../../bindings/LeanStatus";
import { abbreviationsWithPrefix, expandAbbreviation, isTerminator } from "./abbreviations";
import { finishedRevision, statusSentence, statusView } from "./status";

describe("Lean status vocabulary", () => {
  const toolchain = "leanprover/lean4:v4.35.0";
  const cases: [LeanStatus, string, string][] = [
    [
      { kind: "checked", revision: 3, toolchain, warnings: 0, axioms: ["propext"], declarations: [], declarationCount: 0 },
      "succeeded",
      "Checked",
    ],
    [{ kind: "checkedWithSorry", revision: 2, toolchain, count: 1, locations: [], warnings: 0 }, "warning", "Incomplete (sorry)"],
    [{ kind: "errors", revision: 1, toolchain, count: 2, sorryCount: 1 }, "failed", "2 errors"],
    [{ kind: "checking", revision: 4, toolchain, starting: false, auditing: false, processing: [] }, "running", "Checking"],
    [{ kind: "checking", revision: 4, toolchain, starting: false, auditing: true, processing: [] }, "running", "Auditing axioms"],
    [{ kind: "checkedWithAxioms", revision: 5, toolchain, axioms: ["cheat"], declarations: [], warnings: 0 }, "warning", "Checked with axioms"],
    [{ kind: "reliesOnCompiledCode", revision: 5, toolchain, axioms: ["x._native.native_decide.ax_1_1"], declarations: [], warnings: 0 }, "warning", "Relies on compiled code"],
    [{ kind: "partiallyChecked", revision: 5, toolchain, line: 1, reason: "Lean stopped elaborating at `#exit` on line 2." }, "warning", "Partially checked"],
    [{ kind: "unverified", revision: 5, toolchain, reason: "The file disables kernel type checking." }, "warning", "Not verified"],
    [{ kind: "serverUnavailable", reason: "too many interactive sessions are running" }, "unavailable", "Lean not started"],
    [{ kind: "toolchainMissing", toolchain: "leanprover/lean4:v4.0.0", install: "leanprover/lean4:v4.0.0", guidance: "Install the pinned toolchain." }, "unavailable", "Toolchain missing"],
    [{ kind: "untrusted", workspaceRoot: "C:\\p" }, "unavailable", "Untrusted"],
    [{ kind: "serverCrashed", message: "m", restarting: true, attempts: 1 }, "failed", "Server crashed"],
  ];

  it.each(cases)("maps %o to a distinct pill", (status, pill, label) => {
    const view = statusView(status);
    expect(view.status).toBe(pill);
    expect(view.label).toBe(label);
    expect(statusSentence(status).length).toBeGreaterThan(10);
  });

  it("keeps checked, incomplete, and failing proofs distinct", () => {
    const labels = new Set(cases.map(([s]) => statusView(s).label));
    expect(labels.size).toBe(cases.length);
    expect(statusView(cases[0]![0]).detail).toBe("rev 3");
    expect(finishedRevision(cases[1]![0])).toBe(2);
    expect(finishedRevision(cases[3]![0])).toBeNull();
    // Only the clean result is "Checked"; every other finished status says why it is not.
    const checkedLabels = cases.filter(([s]) => statusView(s).label === "Checked").map(([s]) => s.kind);
    expect(checkedLabels).toEqual(["checked"]);
    expect(statusSentence(cases[0]![0])).toContain("only the standard axioms `propext`");
  });
});

describe("Lean abbreviations", () => {
  it("expands known abbreviations at the end of the text", () => {
    expect(expandAbbreviation("theorem t : \\forall")).toEqual({ start: 12, replacement: "∀" });
    expect(expandAbbreviation("x \\to")).toEqual({ start: 2, replacement: "→" });
    expect(expandAbbreviation("\\alpha")).toEqual({ start: 0, replacement: "α" });
    expect(expandAbbreviation("(\\N")).toEqual({ start: 1, replacement: "ℕ" });
  });

  it("ignores unknown names, escaped backslashes, and plain text", () => {
    expect(expandAbbreviation("\\notAnAbbreviation")).toBeNull();
    expect(expandAbbreviation("a \\\\to")).toBeNull();
    expect(expandAbbreviation("alpha")).toBeNull();
    expect(expandAbbreviation("\\alpha x")).toBeNull();
  });

  it("recognizes terminators and lists completions", () => {
    for (const t of [" ", "\t", "(", ")", ",", "\n", "\r\n  "]) expect(isTerminator(t)).toBe(true);
    for (const t of ["a", "ab", "_", "\\"]) expect(isTerminator(t)).toBe(false);
    const fa = abbreviationsWithPrefix("fo");
    expect(fa[0]).toEqual(["forall", "∀"]);
  });
});
