// The Graph's label tables are the single wording for states other surfaces
// also name (QC audit UI-3, UI-5, UI-15). Each is defined once; the surfaces
// that show it import it.
import { describe, expect, it } from "vitest";

import type { Entity } from "../../bindings/Entity";
import { BINDING_BADGE_LABEL, evidenceKindLabel, freshnessView, LIFECYCLE_LABEL } from "./model";
import { lifecycleBadges } from "./projection";
import { syncStateLabel } from "./status";
import { EXECUTION_CEILING_LABEL } from "../workflows/runModel";

describe("single-sourced graph labels", () => {
  it("names an item's lifecycle the same on the canvas badge and in the inspectors", () => {
    expect(LIFECYCLE_LABEL).toEqual({ tombstoned: "Removed from source", archived: "Archived", discarded: "Discarded" });
    for (const lifecycle of ["tombstoned", "archived", "discarded"] as const) {
      expect(lifecycleBadges({ lifecycle } as Entity).map((b) => b.label)).toEqual([LIFECYCLE_LABEL[lifecycle]]);
    }
    expect(lifecycleBadges({ lifecycle: "active" } as Entity)).toEqual([]);
  });

  it("words bindings, evidence kinds and freshness once", () => {
    expect(BINDING_BADGE_LABEL).toEqual({ missing: "Binding missing", ambiguous: "Ambiguous binding", unresolved: "Unresolved binding" });
    expect(evidenceKindLabel("formalCheck")).toBe("Formal check");
    expect(evidenceKindLabel("computational")).toBe("Computational");
    expect(freshnessView(null).label).toBe("No evidence");
    expect(freshnessView({ kind: "current" })).toMatchObject({ label: "Current", standalone: "Current", status: "idle" });
    expect(freshnessView({ kind: "stale", reasons: [] })).toMatchObject({ label: "Stale", standalone: "Stale", status: "stale" });
    expect(freshnessView({ kind: "unknown", reason: "r" })).toMatchObject({ label: "Unknown", standalone: "Freshness unknown", status: "unavailable" });
  });

  it("words graph synchronization once (never current while pending or unavailable)", () => {
    expect(syncStateLabel({ state: "synchronized", pending: 0, checkpointMs: 1 })).toBe("Graph synchronized");
    expect(syncStateLabel({ state: "reconciling", pending: 3, checkpointMs: 1 })).toBe("Graph reconciling");
    expect(syncStateLabel({ state: "pending", pending: 2, checkpointMs: null })).toBe("Reconciliation pending");
    expect(syncStateLabel({ state: "pending", pending: 0, checkpointMs: 5 })).toBe("Reconciliation pending");
    expect(syncStateLabel({ state: "pending", pending: 0, checkpointMs: null })).toBe("Not yet reconciled");
    expect(syncStateLabel({ state: "unavailable", pending: 0, checkpointMs: null })).toBe("Synchronization unavailable");
  });

  it("words the workflow execution ceiling once, keeping restricted and native apart", () => {
    expect(EXECUTION_CEILING_LABEL).toEqual({
      none: "No code runs",
      restricted: "Restricted container only",
      trustedNative: "Trusted native execution allowed (your account's privileges)",
    });
  });
});
