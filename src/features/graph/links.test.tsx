// A formal link's standing, on every surface that shows it or a verdict that
// arrives through it: a suggested link and a reviewed link whose statement
// changed (or cannot be located) never read like a standing review, the
// verdict keeps its exact word with the qualifier, and only the user's
// Confirm clears the flag.
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./reveal", () => ({ revealSource: vi.fn() }));
vi.mock("../lean/evidence", () => ({ leanEvidenceCommands: { associate: vi.fn() } }));
const opIpc = vi.hoisted(() => ({ plan: vi.fn(), apply: vi.fn(), restore: vi.fn() }));
vi.mock("../operations/ipc", () => ({ opCommands: opIpc, opEvents: { projectOp: vi.fn(async () => () => {}) } }));

import type { LinkReview } from "../../bindings/LinkReview";
import type { MathRow } from "../../bindings/MathRow";
import { ipcGraphApi, setGraphApi } from "./api";
import { EdgeLabelContent } from "./canvas/RelationEdge";
import { createFakeGraph, entity, fakeVocabulary, relation, type FakeGraph } from "./fakeApi";
import { EntityInspector, type InspectorActions } from "./Inspector";
import { DependsOnSection, mathBadges, MathSections, VerificationTable } from "./Mathematics";
import { indexVocabulary, verdictThroughLink, type SavedView } from "./model";
import { project } from "./projection";
import { RelationReviewNote, RelationStatus } from "./RelationStatus";
import { resetGraphForTests } from "./store";

const CHANGED: LinkReview = {
  state: "statementChanged",
  reason: "The statement changed since this link was reviewed. Confirm the link to record the current statement, or reject it.",
};
const MISSING: LinkReview = {
  state: "statementMissing",
  reason: "The statement cannot be located, so the review cannot be compared with it (main.typ: The bound text was changed or removed). Rebind the statement to its current text, then confirm the link. Until it is rebound the link cannot be confirmed, only rejected.",
};
const UNRECORDED: LinkReview = {
  state: "unrecorded",
  reason: "The statement text this review covered was not recorded, so whether the statement changed since is unknown. Confirm the link to record the current statement.",
};
const CURRENT: LinkReview = { state: "current", reason: null };

const vocabulary = indexVocabulary(fakeVocabulary());
const thm = entity("thm", "theorem", "Sum of odd numbers");
const decl = entity("decl", "formalDeclaration", "oddSum_eq_sq");

function row(patch: Partial<MathRow>): MathRow {
  return {
    entity: thm,
    binding: null,
    informal: "proofLinked",
    formalAssociation: "reviewed",
    associationReason: null,
    formalDeclarations: ["decl"],
    verdict: "checked",
    verdictAssociation: "reviewed",
    freshness: { kind: "current" },
    formalDependencies: null,
    evidence: null,
    imported: false,
    ...patch,
  };
}

afterEach(() => {
  cleanup();
  resetGraphForTests();
  setGraphApi(ipcGraphApi);
});

describe("a formal link's status", () => {
  it("reads Needs re-review instead of accepted when the statement changed or cannot be located", () => {
    for (const review of [CHANGED, MISSING]) {
      const { container, unmount } = render(<RelationStatus status="accepted" review={review} />);
      const badge = container.querySelector(".ui-badge")!;
      expect(badge.textContent).toBe("Needs re-review");
      expect(badge.getAttribute("data-tone")).toBe("warning");
      expect(badge.getAttribute("data-review")).toBe(review.state);
      expect(container.textContent).not.toMatch(/accepted/i);
      unmount();
    }
  });

  it("says when the reviewed statement was never recorded, without flagging it as changed", () => {
    const { container } = render(<RelationStatus status="accepted" review={UNRECORDED} form="text" />);
    expect(container.textContent).toBe("Accepted · reviewed statement not recorded");
    expect(container.querySelector(".ui-badge")).toBeNull();
  });

  it("is unchanged for a standing review, other statuses, and other relations", () => {
    expect(render(<RelationStatus status="accepted" review={CURRENT} />).container.textContent).toBe("· accepted");
    cleanup();
    expect(render(<RelationStatus status="accepted" />).container.textContent).toBe("· accepted");
    cleanup();
    // A suggestion stays a suggestion whatever else is passed.
    expect(render(<RelationStatus status="suggested" review={CHANGED} />).container.textContent).toBe("Suggested");
  });

  it("states the reason as visible text, not only as a tooltip", () => {
    render(<RelationReviewNote review={CHANGED} />);
    expect(screen.getByTestId("graph-link-review").textContent).toBe(CHANGED.reason);
    cleanup();
    expect(render(<RelationReviewNote review={CURRENT} />).container.textContent).toBe("");
  });
});

describe("a verdict shown beside a statement through its link", () => {
  it("keeps the exact verdict word and names the link's standing", () => {
    expect(verdictThroughLink("checked", "reviewed")).toMatchObject({ label: "Checked", qualified: false });
    expect(verdictThroughLink("checked", "reviewUnrecorded")).toMatchObject({ label: "Checked", qualified: false });
    expect(verdictThroughLink("checked", null)).toMatchObject({ label: "Checked", qualified: false });
    expect(verdictThroughLink("checked", "suggested")).toMatchObject({ label: "Checked (suggested link)", qualified: true, status: "succeeded" });
    expect(verdictThroughLink("checkedWithAxioms", "needsReReview")).toMatchObject({ label: "Checked with axioms (link needs re-review)", qualified: true });
    expect(verdictThroughLink("errors", "suggested").label).toBe("Errors (suggested link)");
    expect(verdictThroughLink("checked", "suggested").sentence).toMatch(/only suggested: nobody has reviewed/);
  });

  it("node badges: provisional (dashed) and labelled, never the plain verdict", () => {
    const badge = (patch: Partial<MathRow>) => {
      const b = mathBadges(row(patch))[0]!;
      return [b.label, b.tone, b.dashed ?? false];
    };
    expect(badge({})).toEqual(["Checked", "success", false]);
    expect(badge({ formalAssociation: "suggested", verdictAssociation: "suggested" })).toEqual(["Checked (suggested link)", "success", true]);
    expect(badge({ formalAssociation: "needsReReview", verdictAssociation: "needsReReview" })).toEqual(["Checked (link needs re-review)", "success", true]);
    expect(badge({ entity: decl, verdictAssociation: null })).toEqual(["Checked", "success", false]);
  });

  it("Verification table: the association and the verdict are both qualified", () => {
    const rows = [
      row({}),
      row({ entity: entity("s", "lemma", "Suggested one"), formalAssociation: "suggested", verdictAssociation: "suggested" }),
      row({ entity: entity("c", "lemma", "Changed one"), formalAssociation: "needsReReview", associationReason: CHANGED.reason, verdictAssociation: "needsReReview" }),
      row({ entity: entity("u", "lemma", "Legacy one"), formalAssociation: "reviewUnrecorded", associationReason: UNRECORDED.reason, verdictAssociation: "reviewUnrecorded" }),
    ];
    render(<VerificationTable rows={rows} vocabulary={vocabulary} selected={null} onSelect={() => {}} />);
    const line = (id: string) => document.querySelector(`[data-entity-id="${id}"]`) as HTMLElement;
    expect(line("thm").textContent).toMatch(/Reviewed association \(1\)Checked(?! \()/);
    expect(within(line("thm")).queryByTestId("graph-verdict-qualified")).toBeNull();

    expect(line("s").textContent).toMatch(/Suggested association \(1\)/);
    const suggested = within(line("s")).getByTestId("graph-verdict-qualified");
    expect(suggested.textContent).toBe("Checked (suggested link)");
    expect(suggested.getAttribute("data-dashed")).toBe("true");
    expect(line("s").getAttribute("data-verdict")).toBe("checked");
    expect(line("s").getAttribute("data-verdict-link")).toBe("suggested");

    expect(line("c").textContent).toMatch(/Association needs re-review \(1\)/);
    expect(line("c").textContent).not.toMatch(/Reviewed association/);
    expect(within(line("c")).getByTestId("graph-verdict-qualified").textContent).toBe("Checked (link needs re-review)");
    expect(line("c").getAttribute("data-association")).toBe("needsReReview");

    expect(line("u").textContent).toMatch(/Reviewed association \(1\) · statement not recorded/);
    expect(within(line("u")).queryByTestId("graph-verdict-qualified")).toBeNull();
  });
});

describe("canvas edges", () => {
  it("label a reviewed link that needs re-review", () => {
    const flagged = { ...relation("f1", "formalizes", "decl", "thm", "accepted"), review: CHANGED };
    const view = { hidden: [], collapsed: [], filters: { kinds: [], relationKinds: [], text: "", showInactive: false, depth: null } } as unknown as SavedView;
    const edge = project({ entities: [thm, decl], relations: [flagged] }, vocabulary, view).edges[0]!;
    expect(edge.attention).toBe("needs re-review");
    expect(edge.description).toBe("oddSum_eq_sq formalizes Sum of odd numbers, needs re-review");
    const { container } = render(<EdgeLabelContent label={edge.label} status={edge.status} attention={edge.attention} />);
    expect(container.textContent).toBe("formalizes· needs re-review");
    const plain = project({ entities: [thm, decl], relations: [{ ...flagged, review: CURRENT }] }, vocabulary, view).edges[0]!;
    expect(plain.attention).toBeUndefined();
  });
});

// XR-D-01: a rejected dependency is not a dependency; a suggested one is labelled.
describe("Depends on (Mathematics inspector)", () => {
  const lem = entity("lem", "lemma", "Nonnegative energy");
  const def = entity("def", "definition", "Pair energy");
  const hyp = entity("hyp", "assumption", "Finite pairs");
  const listed = async () => {
    await waitFor(() => expect(screen.queryByText("Tracing dependencies…")).toBeNull(), { timeout: 3000 });
    return screen.queryAllByTestId("graph-depends-on").map((li) => [li.getAttribute("data-entity-id"), li.getAttribute("data-status")]);
  };

  it("leaves out what is reached only through a rejected suggestion, directly or transitively", async () => {
    setGraphApi(
      createFakeGraph({
        entities: [thm, lem, def, hyp],
        relations: [relation("r1", "dependsOn", "thm", "lem"), relation("r2", "dependsOn", "lem", "def", "rejected"), relation("r3", "assumes", "lem", "hyp")],
      }).api,
    );
    render(<DependsOnSection entityId="thm" vocabulary={vocabulary} onSelectEntity={() => {}} />);
    expect(await listed()).toEqual([
      ["hyp", "asserted"],
      ["lem", "asserted"],
    ]);
    cleanup();
    render(<DependsOnSection entityId="lem" vocabulary={vocabulary} onSelectEntity={() => {}} />);
    expect(await listed()).toEqual([["hyp", "asserted"]]);
    expect(screen.queryByText("Pair energy")).toBeNull();
  });

  it("labels an item reached only through suggested links as suggested", async () => {
    setGraphApi(
      createFakeGraph({
        entities: [thm, lem, def, hyp],
        relations: [relation("r1", "dependsOn", "thm", "lem", "suggested"), relation("r2", "dependsOn", "lem", "def"), relation("r3", "dependsOn", "thm", "hyp"), relation("r4", "assumes", "thm", "hyp", "suggested")],
      }).api,
    );
    render(<DependsOnSection entityId="thm" vocabulary={vocabulary} onSelectEntity={() => {}} />);
    const rows = await listed();
    expect(Object.fromEntries(rows)).toEqual({ lem: "suggested", def: "suggested", hyp: "asserted" });
    const lemRow = screen.getAllByTestId("graph-depends-on").find((li) => li.getAttribute("data-entity-id") === "lem")!;
    expect(within(lemRow).getByText("Suggested").closest("[title]")?.getAttribute("title")).toBe("Suggested, not yet accepted");
    const hypRow = screen.getAllByTestId("graph-depends-on").find((li) => li.getAttribute("data-entity-id") === "hyp")!;
    expect(within(hypRow).queryByText("Suggested")).toBeNull();
  });
});

describe("the inspector of a statement whose reviewed link needs re-review", () => {
  let fake: FakeGraph;
  const actions: InspectorActions = { canWrite: true, onSelectEntity: () => {}, onRelate: () => {} };

  beforeEach(() => {
    fake = createFakeGraph({
      entities: [thm, decl],
      relations: [{ ...relation("f1", "formalizes", "decl", "thm", "accepted"), review: CHANGED }],
    });
    setGraphApi(fake.api);
  });

  it("shows the state and the reason in the evidence pointer and the relationship, and Confirm records the review", async () => {
    render(<EntityInspector entityId="thm" vocabulary={vocabulary} actions={actions} />);
    const pointer = await screen.findByTestId("graph-evidence-formal", {}, { timeout: 3000 });
    const declaration = within(pointer).getByTestId("graph-evidence-formal-declaration");
    expect(declaration.getAttribute("data-review")).toBe("statementChanged");
    expect(within(declaration).getByText("Needs re-review")).toBeTruthy();
    expect(within(declaration).getByTestId("graph-link-review").textContent).toBe(CHANGED.reason);

    const link = screen.getByTestId("graph-relation");
    expect(within(link).getByText("Needs re-review")).toBeTruthy();
    expect(within(link).getByTestId("graph-link-review").textContent).toMatch(/^The statement changed since this link was reviewed\./);
    expect(within(link).getByRole("button", { name: "Reject link" })).toBeTruthy();
    fireEvent.click(within(link).getByRole("button", { name: "Confirm link" }));
    await waitFor(() => expect(fake.calls.some((c) => c.method === "apply")).toBe(true));
    const proposed = fake.calls.find((c) => c.method === "propose")!.args[0] as { operations: unknown[] };
    expect(proposed.operations).toEqual([{ op: "setRelationStatus", relation: "f1", status: "accepted" }]);
    await waitFor(() => expect(screen.queryByText("Needs re-review")).toBeNull());
    expect(screen.queryByRole("button", { name: "Confirm link" })).toBeNull();
  });

  it("does not offer to confirm a link whose statement cannot be located, and says what to do first", async () => {
    fake = createFakeGraph({
      entities: [thm, decl],
      relations: [{ ...relation("f1", "formalizes", "decl", "thm", "accepted"), review: MISSING }],
    });
    setGraphApi(fake.api);
    render(<EntityInspector entityId="thm" vocabulary={vocabulary} actions={actions} />);
    const link = await screen.findByTestId("graph-relation", {}, { timeout: 3000 });
    expect(within(link).getByText("Needs re-review")).toBeTruthy();
    expect(within(link).getByTestId("graph-link-review").textContent).toMatch(/Rebind the statement to its current text, then confirm the link\./);
    const confirm = within(link).getByRole("button", { name: "Confirm link" }) as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    fireEvent.click(confirm);
    expect((within(link).getByRole("button", { name: "Reject link" }) as HTMLButtonElement).disabled).toBe(false);
    expect(fake.calls.some((c) => c.method === "propose")).toBe(false);
  });
});

// RL-7: a rejected proof or formal link is not a link of the statement; the
// Mathematics inspector neither lists nor counts it (as the Verification
// table, the Graph inspector's evidence pointer, and the service do not).
describe("Mathematics inspector sections", () => {
  const prf = entity("prf", "proof", "Proof by induction");
  const old = entity("old", "proof", "Withdrawn proof");
  const other = entity("other", "formalDeclaration", "oddSum_wrong");

  it("leaves rejected proof and formal links out of the lists and their counts", async () => {
    const fake = createFakeGraph({
      entities: [thm, decl, prf, old, other],
      relations: [
        relation("p1", "proofOf", "prf", "thm", "accepted"),
        relation("p2", "proofOf", "old", "thm", "rejected"),
        relation("f1", "formalizes", "decl", "thm", "suggested"),
        relation("f2", "formalizes", "other", "thm", "rejected"),
      ],
    });
    setGraphApi(fake.api);
    const detail = await fake.api.entity("thm");
    render(<MathSections detail={detail} row={undefined} vocabulary={vocabulary} onSelectEntity={() => {}} declarations={[decl, other]} canWrite />);
    expect(screen.getAllByTestId("graph-proof").map((li) => li.getAttribute("data-entity-id"))).toEqual(["prf"]);
    expect(screen.getAllByTestId("graph-formalization").map((li) => li.getAttribute("data-entity-id"))).toEqual(["decl"]);
    expect(screen.queryByText("Withdrawn proof")).toBeNull();
    expect(screen.queryByText("oddSum_wrong")).toBeNull();
    const proofs = screen.getByText("Proofs targeting this").closest(".ui-section") as HTMLElement;
    expect(proofs.textContent).toMatch(/Proofs targeting this\s*1/);
  });

  it("says no proof is linked when the only proof link was rejected", async () => {
    const fake = createFakeGraph({
      entities: [thm, old, other],
      relations: [relation("p2", "proofOf", "old", "thm", "rejected"), relation("f2", "formalizes", "other", "thm", "rejected")],
    });
    setGraphApi(fake.api);
    const detail = await fake.api.entity("thm");
    render(<MathSections detail={detail} row={undefined} vocabulary={vocabulary} onSelectEntity={() => {}} declarations={[other]} canWrite />);
    expect(screen.getByText("No proof is linked to this statement.")).toBeTruthy();
    expect(screen.getByText("No formal declaration is associated with this item.")).toBeTruthy();
    expect(screen.queryByTestId("graph-proof")).toBeNull();
    // The proof's own view: a rejected target is not what it proves.
    cleanup();
    const proofDetail = await fake.api.entity("old");
    render(<MathSections detail={proofDetail} row={undefined} vocabulary={vocabulary} onSelectEntity={() => {}} declarations={[other]} canWrite />);
    expect(screen.getByText("This proof targets no statement yet.")).toBeTruthy();
  });
});
