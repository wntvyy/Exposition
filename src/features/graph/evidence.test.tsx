// Where evidence is, and what a file's change impact covers, as the Graph
// inspector and the Dependencies panel say it (pre-review walkthrough: a
// statement with an associated, checked Lean declaration read "No evidence
// recorded" with no pointer; a file's impact read "Nothing in the graph
// depends on it" although items are located in the file).
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./reveal", () => ({ revealSource: vi.fn() }));
const opIpc = vi.hoisted(() => ({ plan: vi.fn(), apply: vi.fn(), restore: vi.fn() }));
vi.mock("../operations/ipc", () => ({ opCommands: opIpc, opEvents: { projectOp: vi.fn(async () => () => {}) } }));

import { ipcGraphApi, setGraphApi } from "./api";
import { createFakeGraph, entity, fakeVocabulary, relation, type FakeGraph } from "./fakeApi";
import { EntityInspector, ImpactSection, type InspectorActions } from "./Inspector";
import { indexVocabulary } from "./model";
import { resetGraphForTests } from "./store";

let fake: FakeGraph;
const vocabulary = indexVocabulary(fakeVocabulary());
const selected: string[] = [];
const actions: InspectorActions = {
  canWrite: true,
  onSelectEntity: (id) => selected.push(id),
  onRelate: () => {},
};

beforeEach(() => {
  selected.length = 0;
  fake = createFakeGraph({
    entities: [
      entity("thm", "theorem", "Sum of odd numbers", { attributes: { path: "main.typ" } }),
      entity("prf", "proof", "Proof by induction", { attributes: { path: "main.typ" } }),
      entity("decl", "formalDeclaration", "oddSum_eq_sq"),
      entity("other", "formalDeclaration", "oddSum"),
      entity("gone", "formalDeclaration", "rejected_decl"),
      entity("doc", "resource", "main.typ", { roles: ["manuscript"], key: "main.typ" }),
    ],
    relations: [
      relation("f1", "formalizes", "decl", "thm", "accepted"),
      relation("f2", "formalizes", "other", "thm", "suggested"),
      relation("f3", "formalizes", "gone", "thm", "rejected"),
      relation("p1", "proofOf", "prf", "thm", "suggested"),
    ],
  });
  setGraphApi(fake.api);
});

afterEach(() => {
  cleanup();
  resetGraphForTests();
  setGraphApi(ipcGraphApi);
});

describe("Evidence of a statement with associated formal declarations", () => {
  it("points at the declarations with the state of each association, never as the statement's own evidence", async () => {
    render(<EntityInspector entityId="thm" vocabulary={vocabulary} actions={actions} />);
    const pointer = await screen.findByTestId("graph-evidence-formal", {}, { timeout: 3000 });
    expect(screen.getByText("No evidence recorded.")).toBeTruthy();
    expect(pointer.textContent).toMatch(/recorded for a formal declaration, not for the item it formalizes/);
    expect(pointer.textContent).toMatch(/not a machine proof/);
    const rows = within(pointer).getAllByTestId("graph-evidence-formal-declaration");
    expect(rows.map((r) => [r.getAttribute("data-entity-id"), r.getAttribute("data-status")])).toEqual([
      ["decl", "accepted"],
      ["other", "suggested"],
    ]);
    expect(within(rows[1]!).getByText("Suggested")).toBeTruthy();
    fireEvent.click(within(rows[0]!).getByRole("button", { name: "oddSum_eq_sq" }));
    expect(selected).toEqual(["decl"]);
  });

  it("says how a declaration's verdict gets recorded when it has none", async () => {
    render(<EntityInspector entityId="decl" vocabulary={vocabulary} actions={actions} />);
    const how = await screen.findByTestId("graph-evidence-how", {}, { timeout: 3000 });
    expect(how.textContent).toMatch(/A Lean check alone records nothing here/);
    expect(how.textContent).toMatch(/Record evidence in the Lean panel/);
    expect(screen.queryByTestId("graph-evidence-formal")).toBeNull();
  });

  it("leaves the associations to the Mathematics view's own section", async () => {
    render(
      <EntityInspector entityId="thm" vocabulary={vocabulary} actions={actions}>
        {() => <p data-testid="math-sections">Formal verification</p>}
      </EntityInspector>,
    );
    await screen.findByTestId("math-sections", {}, { timeout: 3000 });
    expect(screen.queryByTestId("graph-evidence-formal")).toBeNull();
  });
});

describe("Change impact of a file", () => {
  it("lists the items located in it and says their dependents are not followed", async () => {
    render(<ImpactSection entityId="doc" labelOf={(id) => id} />);
    fireEvent.click(screen.getByRole("button", { name: "What needs review if this changes?" }));
    await waitFor(() => expect(screen.getAllByTestId("graph-impact-item").length).toBe(2), { timeout: 3000 });
    const items = screen.getAllByTestId("graph-impact-item");
    expect(items.map((i) => i.getAttribute("data-action"))).toEqual(["informational", "informational"]);
    expect(items[0]!.textContent).toMatch(/is located in “main\.typ”, which changed; check whether it changed/);
    expect(items[0]!.textContent).not.toMatch(/via/);
    expect(screen.getByTestId("graph-impact-file-scope").textContent).toMatch(/listed without what depends on them/);
  });

  it("keeps the plain answer for an item nothing depends on", async () => {
    render(<ImpactSection entityId="prf" labelOf={(id) => id} />);
    fireEvent.click(screen.getByRole("button", { name: "What needs review if this changes?" }));
    await screen.findByText("Nothing in the graph depends on it.", {}, { timeout: 3000 });
    expect(screen.queryByTestId("graph-impact-file-scope")).toBeNull();
  });
});
