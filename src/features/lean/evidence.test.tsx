// Lean evidence in the infoview: verdicts keep their distinctions, a reviewed
// association is shown as a correspondence (never a proof of equivalence),
// freshness is shown separately (history after an edit), and evidence can be
// recorded only for declarations the audit listed.
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DeclarationEvidence } from "../../bindings/DeclarationEvidence";
import type { LeanAuditRecord } from "../../bindings/LeanAuditRecord";
import type { LeanDocumentState } from "../../bindings/LeanDocumentState";

vi.mock("./leanMonaco", () => ({ revealInEditor: vi.fn() }));
vi.mock("../../lib/graphIpc", () => ({ graphCommands: { snapshot: vi.fn() }, graphEvents: {} }));

const { verdictView } = await import("./evidence");
const { DeclarationEvidenceControls } = await import("./FormalEvidence");

const state = { document: "doc-1", path: "LeanBasic/Defs.lean", revision: 2 } as unknown as LeanDocumentState;

const audit = {
  document: "doc-1",
  path: "LeanBasic/Defs.lean",
  revision: 2,
  contentHash: "b".repeat(64),
  toolchain: "leanprover/lean4:v4.35.0",
  toolchainVersion: null,
  status: { kind: "checked" },
  declarations: [{ name: "LeanBasic.double_zero", kind: "theorem", statement: "x", range: null, line: null, axioms: [], verdict: "checked" }],
  declarationCount: 1,
  truncated: false,
  finishedAtMs: 0,
} as unknown as LeanAuditRecord;

function evidence(revision: number, digest: string, freshness: unknown): DeclarationEvidence {
  return {
    entity: "ent-d",
    name: "LeanBasic.double_zero",
    evidence: [
      {
        evidence: {
          formal: {
            declaration: "LeanBasic.double_zero",
            fileVerdict: "checked",
            declarationVerdict: "checked",
            toolchain: "leanprover/lean4:v4.35.0",
            sourceDigest: digest,
            revision,
            dependencies: { kind: "unavailable", reason: "the Lean audit does not report declaration dependencies" },
          },
        },
        freshness,
      },
    ],
    associations: [
      {
        relation: "rel-1",
        target: { id: "ent-c", kind: "claim", label: "Doubling zero gives zero" },
        status: "accepted",
        note: null,
      },
    ],
  } as unknown as DeclarationEvidence;
}

afterEach(cleanup);

describe("Lean evidence", () => {
  it("keeps every verdict distinct", () => {
    const labels = (["checked", "checkedWithAxioms", "incomplete", "reliesOnCompiledCode", "partiallyChecked", "notVerified", "errors"] as const).map(
      (v) => verdictView(v).label,
    );
    expect(new Set(labels).size).toBe(7);
    expect(verdictView("checked").status).toBe("succeeded");
    expect(verdictView("checkedWithAxioms").status).not.toBe("succeeded");
  });

  it("shows the association as a reviewed correspondence and historical freshness separately", () => {
    render(
      <DeclarationEvidenceControls
        state={state}
        name="LeanBasic.double_zero"
        audit={audit}
        evidence={evidence(1, "a".repeat(64), { kind: "stale", reasons: ["LeanBasic/Defs.lean changed since revision 1 was checked"] })}
      />,
    );
    expect(screen.getByTestId("lean-decl-association").textContent).toMatch(/reviewed correspondence \(not a proof of equivalence\)/);
    expect(screen.getByTestId("lean-evidence-latest").getAttribute("data-freshness")).toBe("stale");
    expect(screen.getByTestId("lean-evidence-history").textContent).toMatch(/changed since revision 1/);
    // The current audit (revision 2) has no evidence yet: it can be recorded.
    expect(screen.getByRole("button", { name: "Record evidence" })).toBeTruthy();
  });

  it("offers no recording when this audit's evidence exists, and none for unlisted declarations", () => {
    const { unmount } = render(
      <DeclarationEvidenceControls
        state={state}
        name="LeanBasic.double_zero"
        audit={audit}
        evidence={evidence(2, "b".repeat(64), { kind: "unknown", reason: "dependencies are not fingerprinted" })}
      />,
    );
    expect(screen.queryByRole("button", { name: "Record evidence" })).toBeNull();
    expect(screen.getByTestId("lean-evidence-latest").getAttribute("data-freshness")).toBe("unknown");
    unmount();
    render(<DeclarationEvidenceControls state={state} name="LeanBasic.other" audit={audit} evidence={undefined} />);
    expect(screen.queryByRole("button", { name: "Record evidence" })).toBeNull();
    expect(screen.getByRole("button", { name: "Associate with…" }).hasAttribute("disabled")).toBe(true);
  });
});
