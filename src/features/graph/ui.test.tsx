import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { ConfirmHost } from "../../ui/components";

// Source navigation opens Monaco; not needed here.
vi.mock("./reveal", () => ({ revealSource: vi.fn() }));

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
    // The observed reference is shown with its status and cannot be removed by hand.
    const observed = await within(inspector).findByText("Observed");
    const item = observed.closest('[data-testid="graph-relation"]') as HTMLElement;
    expect(item.getAttribute("data-status")).toBe("observed");
    expect(within(item).queryByRole("button", { name: "Remove relationship" })).toBeNull();
    // Keyboard path to a relationship: Relate to… opens the typed dialog with a target picker.
    fireEvent.click(within(inspector).getByRole("button", { name: "Relate to…" }));
    expect(await screen.findByTestId("graph-relate-dialog")).toBeTruthy();
    expect(screen.getByText("Related item", { selector: ".ui-field__label" })).toBeTruthy();
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
});
