import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { ConfirmHost } from "../../ui/components";

// Source navigation opens Monaco; not needed here.
vi.mock("./reveal", () => ({ revealSource: vi.fn() }));
// P1 operations: the backend contract, mocked at the operation IPC boundary.
const opIpc = vi.hoisted(() => ({ plan: vi.fn(), apply: vi.fn(), restore: vi.fn() }));
vi.mock("../operations/ipc", () => ({ opCommands: opIpc, opEvents: { projectOp: vi.fn(async () => () => {}) } }));

import { ipcGraphApi, setGraphApi } from "./api";
import { RelateDialog } from "./changes";
import { createFakeGraph, entity, relation, type FakeGraph } from "./fakeApi";
import { indexVocabulary } from "./model";
import { fakeVocabulary } from "./fakeApi";
import { ProjectGraph } from "./ProjectGraph";
import { graph, resetGraphForTests, useGraph } from "./store";
import { createViewStore } from "./viewStore";

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  (globalThis as { DOMMatrixReadOnly?: unknown }).DOMMatrixReadOnly ??= class {
    m22 = 1;
  };
  globalThis.matchMedia ??= ((query: string) => ({
    matches: false,
    media: query,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof matchMedia;
});

let fake: FakeGraph;

beforeEach(() => {
  localStorage.clear();
  fake = createFakeGraph({
    entities: [
      entity("thm", "theorem", "Main theorem", { key: "thm:main" }),
      entity("lem", "lemma", "Key lemma"),
      entity("dir", "folder", "chapters"),
      entity("col", "collection", "Reading list"),
    ],
    relations: [relation("obs", "references", "thm", "lem", "observed")],
  });
  setGraphApi(fake.api);
});

afterEach(() => {
  cleanup();
  resetGraphForTests();
  setGraphApi(ipcGraphApi);
});

const vocabulary = indexVocabulary(fakeVocabulary());

async function chooseOption(label: string, option: RegExp) {
  const field = screen.getByText(label, { selector: ".ui-field__label" }).closest(".ui-select") as HTMLElement;
  fireEvent.click(within(field).getByRole("button"));
  const listbox = await screen.findByRole("listbox");
  fireEvent.click(within(listbox).getByRole("option", { name: option }));
}

describe("typed relationship authoring", () => {
  it("refuses an invalid connection with the backend's reason and never applies it", async () => {
    const onClose = vi.fn();
    render(
      <>
        <ConfirmHost />
        <RelateDialog vocabulary={vocabulary} from={fake.entities[0]!} to={fake.entities[2]!} candidates={fake.entities} onClose={onClose} />
      </>,
    );
    // Theorem → Folder: "depends on" is not listed for this pair; choosing it anyway asks the backend.
    await chooseOption("Relationship", /^depends on \(not listed/);
    const review = await screen.findByTestId("graph-proposal-review");
    await waitFor(() => expect(review.textContent).toMatch(/cannot go from a theorem to a folder/));
    expect(review.getAttribute("data-state")).toBe("invalid");
    expect(review.querySelector('[data-code="graph.endpointRule"]')).not.toBeNull();
    const apply = screen.getByRole("button", { name: "Add relationship" });
    expect(apply.hasAttribute("disabled") || apply.getAttribute("aria-disabled") === "true" || apply.hasAttribute("data-disabled")).toBe(true);
    fireEvent.click(apply);
    expect(fake.calls.some((c) => c.method === "apply")).toBe(false);
    expect(fake.relations).toHaveLength(1);
  });

  it("applies a valid typed relationship only after review", async () => {
    const onClose = vi.fn();
    render(<RelateDialog vocabulary={vocabulary} from={fake.entities[0]!} to={fake.entities[1]!} candidates={fake.entities} onClose={onClose} />);
    const review = await screen.findByTestId("graph-proposal-review");
    await waitFor(() => expect(review.getAttribute("data-state")).toBe("valid"));
    expect(fake.relations).toHaveLength(1);
    const proposal = fake.calls.find((c) => c.method === "propose")!.args[0] as { operations: { kind: string }[] };
    expect(proposal.operations[0]!.kind).toBe("dependsOn");
    fireEvent.click(screen.getByRole("button", { name: "Add relationship" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(fake.relations.map((r) => [r.kind, r.from, r.to])).toContainEqual(["dependsOn", "thm", "lem"]);
  });
});

// RL-6: "is evidence for" needs the kind of support. The dialog asks for it (only kinds a person
// can state; a service's evidence is never offered) instead of sending a change that is always refused.
describe("an evidence relationship", () => {
  const proof = entity("prf", "proof", "Proof by induction");

  it("asks for the kind of support before checking, and adds the relationship with it", async () => {
    fake = createFakeGraph({ entities: [entity("thm", "theorem", "Main theorem"), proof] });
    setGraphApi(fake.api);
    const onClose = vi.fn();
    render(<RelateDialog vocabulary={vocabulary} from={proof} to={fake.entities[0]!} candidates={fake.entities} onClose={onClose} />);
    await chooseOption("Relationship", /^is evidence for$/);
    expect(await screen.findByText("Choose the kind of support.")).toBeTruthy();
    // Nothing that the backend always refuses is sent.
    const sent = () => fake.calls.filter((c) => c.method === "propose").map((c) => (c.args[0] as { operations: { kind: string; evidenceKind: string | null }[] }).operations[0]!);
    await new Promise((r) => setTimeout(r, 250));
    expect(sent().filter((op) => op.kind === "evidenceFor" && !op.evidenceKind)).toEqual([]);

    // Only the kinds a person can state are offered.
    const field = screen.getByText("Kind of support", { selector: ".ui-field__label" }).closest(".ui-select") as HTMLElement;
    fireEvent.click(within(field).getByRole("button"));
    const listbox = await screen.findByRole("listbox");
    const offered = within(listbox).getAllByRole("option").map((o) => o.textContent ?? "");
    expect(offered).toHaveLength(2);
    expect(offered[0]).toMatch(/^Review/);
    expect(offered[1]).toMatch(/^Literature/);
    fireEvent.click(within(listbox).getByRole("option", { name: /^Review/ }));

    const review = await screen.findByTestId("graph-proposal-review");
    await waitFor(() => expect(review.getAttribute("data-state")).toBe("valid"));
    expect(sent().at(-1)).toMatchObject({ kind: "evidenceFor", evidenceKind: "review", evidence: [] });
    fireEvent.click(screen.getByRole("button", { name: "Add relationship" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(fake.relations.map((r) => [r.kind, r.from, r.to, r.evidenceKind])).toEqual([["evidenceFor", "prf", "thm", "review"]]);
  });

  it("sends no evidence kind for any other relationship", async () => {
    fake = createFakeGraph({ entities: [entity("thm", "theorem", "Main theorem"), proof] });
    setGraphApi(fake.api);
    render(<RelateDialog vocabulary={vocabulary} from={proof} to={fake.entities[0]!} candidates={fake.entities} onClose={() => {}} />);
    await chooseOption("Relationship", /^is evidence for$/);
    await chooseOption("Kind of support", /^Literature/);
    await chooseOption("Relationship", /^is a proof of$/);
    expect(screen.queryByText("Kind of support", { selector: ".ui-field__label" })).toBeNull();
    const review = await screen.findByTestId("graph-proposal-review");
    await waitFor(() => expect(review.getAttribute("data-state")).toBe("valid"));
    const last = fake.calls.filter((c) => c.method === "propose").at(-1)!.args[0] as { operations: unknown[] };
    expect(last.operations[0]).toMatchObject({ kind: "proofOf", evidenceKind: null });
  });
});

describe("Project graph table (keyboard alternative)", () => {
  it("lists items, selects one into the inspector, and keeps view-only and semantic actions apart", async () => {
    localStorage.setItem("exposition.graph.mode.project", "table");
    graph.start();
    await waitFor(() => expect(useGraph.getState().vocabulary).not.toBeNull());
    const store = createViewStore("Project", { kind: "project" }, { saveDelayMs: 5 });
    await store.load();
    render(
      <>
        <ConfirmHost />
        <ProjectGraph store={store} />
      </>,
    );
    const grid = await screen.findByRole("grid", { name: "Entities" });
    expect(within(grid).getAllByRole("rowheader").map((c) => c.textContent)).toEqual(["chapters", "Key lemma", "Main theorem", "Reading list"]);

    const row = within(grid).getByRole("rowheader", { name: "Main theorem" }).closest('[role="row"]') as HTMLElement;
    expect(row.getAttribute("data-entity-id")).toBe("thm");
    act(() => row.focus());
    fireEvent.keyDown(row, { key: " ", code: "Space" });
    fireEvent.keyUp(row, { key: " ", code: "Space" });
    const inspector = await screen.findByTestId("graph-entity-inspector");
    expect(inspector.getAttribute("data-entity-id")).toBe("thm");
    // The observed reference is shown with its status (a settled fact: muted "· observed") and cannot be removed by hand.
    const observed = await within(inspector).findByText("· observed");
    const item = observed.closest('[data-testid="graph-relation"]') as HTMLElement;
    expect(item.getAttribute("data-status")).toBe("observed");
    expect(within(item).queryByRole("button", { name: "Delete relationship" })).toBeNull();
    // Keyboard path to a relationship: Relate to… opens the typed dialog with a target picker.
    fireEvent.click(within(inspector).getByRole("button", { name: "Relate to…" }));
    expect(await screen.findByTestId("graph-relate-dialog")).toBeTruthy();
    expect(screen.getByText("Related item", { selector: ".ui-field__label" })).toBeTruthy();
  });

  it("P1: the inspector's operation items show no key hints (the keys act on the canvas/table selection, not in the inspector)", async () => {
    localStorage.setItem("exposition.graph.mode.project", "table");
    graph.start();
    await waitFor(() => expect(useGraph.getState().vocabulary).not.toBeNull());
    const store = createViewStore("Project", { kind: "project" }, { saveDelayMs: 5 });
    await store.load();
    render(<ProjectGraph store={store} />);
    const grid = await screen.findByRole("grid", { name: "Entities" });
    const row = within(grid).getByRole("rowheader", { name: "Reading list" }).closest('[role="row"]') as HTMLElement;
    act(() => row.focus());
    fireEvent.keyDown(row, { key: " ", code: "Space" });
    fireEvent.keyUp(row, { key: " ", code: "Space" });
    const inspector = await screen.findByTestId("graph-entity-inspector");
    fireEvent.click(within(inspector).getByRole("button", { name: "More actions" }));
    const menu = await screen.findByRole("menu", { name: "More actions" });
    const del = within(menu).getByRole("menuitem", { name: /Delete Entity/ });
    expect(del.querySelector(".ui-menu-item__keys")).toBeNull();
    expect(menu.querySelectorAll(".ui-menu-item__keys").length).toBe(0);
  });

  it("hides an item from the view without changing the graph", async () => {
    localStorage.setItem("exposition.graph.mode.project", "table");
    graph.start();
    await waitFor(() => expect(useGraph.getState().vocabulary).not.toBeNull());
    const store = createViewStore("Project", { kind: "project" }, { saveDelayMs: 5 });
    await store.load();
    render(<ProjectGraph store={store} />);
    const grid = await screen.findByRole("grid", { name: "Entities" });
    act(() => store.hide(["lem"]));
    await waitFor(() => expect(within(grid).queryByRole("rowheader", { name: "Key lemma" })).toBeNull());
    expect(screen.getByRole("button", { name: "Show 1 hidden" })).toBeTruthy();
    expect(fake.entities.some((e) => e.id === "lem")).toBe(true);
    expect(fake.calls.some((c) => c.method === "propose" || c.method === "apply")).toBe(false);
    await waitFor(() => expect([...fake.views.values()][0]?.hidden).toEqual(["lem"]));
  });

  it("deletes a manual relationship at once through the operation contract (no generic confirmation); an observed one offers no delete", async () => {
    fake.relations.push(relation("acc", "dependsOn", "thm", "lem"));
    opIpc.plan.mockImplementation(async (request) => ({
      plan: "p1",
      request,
      effects: request.targets.map((target: unknown) => ({ target, description: "Delete the relationship", destination: null, recovery: { kind: "appManaged" } })),
      refusals: [],
      impacts: [],
      needsDecision: false,
      choices: [],
      planHash: "h1",
      expiresAtMs: Date.now() + 60_000,
    }));
    opIpc.apply.mockImplementation(async () => {
      fake.relations.splice(fake.relations.findIndex((r) => r.id === "acc"), 1);
      return {
        plan: "p1",
        outcomes: [{ target: { kind: "graphRelation", relation: "acc" }, status: "succeeded", code: null, message: null, created: null, recovery: { kind: "appManaged" }, restore: null }],
        notices: [],
      };
    });
    localStorage.setItem("exposition.graph.mode.project", "table");
    graph.start();
    await waitFor(() => expect(useGraph.getState().vocabulary).not.toBeNull());
    const store = createViewStore("Project", { kind: "project" }, { saveDelayMs: 5 });
    await store.load();
    render(
      <>
        <ConfirmHost />
        <ProjectGraph store={store} />
      </>,
    );
    fireEvent.click(await screen.findByRole("tab", { name: /Relationships/ }));
    const grid = await screen.findByRole("grid", { name: "Relationships" });
    const row = grid.querySelector('[data-relation-id="acc"]') as HTMLElement;
    act(() => row.focus());
    fireEvent.keyDown(row, { key: " ", code: "Space" });
    fireEvent.keyUp(row, { key: " ", code: "Space" });
    const inspector = await screen.findByTestId("graph-relation-inspector");
    // The relationship's family reads in its edge color (shared category text role).
    const family = inspector.querySelector(".ui-inspector-header .ui-category-text")!;
    expect([family.textContent, family.getAttribute("data-category")]).toEqual(["Mathematical dependencies", "math"]);
    fireEvent.click(within(inspector).getByRole("button", { name: "Delete relationship" }));
    await waitFor(() => expect(opIpc.apply).toHaveBeenCalledWith({ plan: "p1", planHash: "h1", choices: {} }));
    expect(opIpc.plan).toHaveBeenCalledWith({ kind: { kind: "delete", permanent: false }, targets: [{ kind: "graphRelation", relation: "acc" }], actor: { kind: "user" } });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(fake.relations.some((r) => r.id === "acc")).toBe(false);
    // The observed one offers no remove action and says why.
    const observedRow = grid.querySelector('[data-relation-id="obs"]') as HTMLElement;
    act(() => observedRow.focus());
    fireEvent.keyDown(observedRow, { key: " ", code: "Space" });
    fireEvent.keyUp(observedRow, { key: " ", code: "Space" });
    await waitFor(() => expect(screen.getByTestId("graph-relation-inspector").textContent).toMatch(/change the source/));
    expect(within(screen.getByTestId("graph-relation-inspector")).queryByRole("button", { name: "Delete relationship" })).toBeNull();
  });
});
