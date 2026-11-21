import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Evidence } from "../../bindings/Evidence";

// The editor and workspace modules pull in Monaco and Tauri; the inspector only
// needs the caret position and whether a folder is open.
const position = vi.hoisted(() => ({ current: { path: "paper.tex", offset: 12, line: 2, revision: 3, dirty: false, fromEditor: true } as unknown }));
vi.mock("../editor", () => ({ useActiveSourcePosition: () => position.current }));
vi.mock("../workspace/store", () => ({
  useWorkspace: (select: (s: { info: unknown }) => unknown) => select({ info: { root: "C:/p", name: "p", recovery: [] } }),
}));
vi.mock("./reveal", () => ({ revealSource: vi.fn() }));
vi.mock("../../app/pages", () => ({ pages: { show: vi.fn() }, EDITOR_PAGE: "editor" }));

const { ipcGraphApi, setGraphApi } = await import("./api");
const { createFakeGraph, entity, relation } = await import("./fakeApi");
const { DependencyInspector } = await import("./DependencyInspector");
const { resetGraphForTests } = await import("./store");
const { useGraphNavigation, useGraphPage } = await import("./navigation");

beforeEach(() => {
  const fake = createFakeGraph({
    entities: [
      entity("thm", "theorem", "Main theorem", { attributes: { path: "paper.tex" } }),
      entity("lem", "lemma", "Key lemma"),
      entity("ref", "reference", "Rudin 1976"),
      entity("doc", "resource", "paper.tex", { roles: ["manuscript"], key: "paper.tex" }),
    ],
    relations: [relation("d1", "dependsOn", "thm", "lem"), relation("x1", "references", "thm", "ref", "observed")],
  });
  setGraphApi(fake.api);
});

afterEach(() => {
  cleanup();
  resetGraphForTests();
  setGraphApi(ipcGraphApi);
});

describe("Dependency inspector", () => {
  it("shows the entities at the caret with dependencies and references apart, coverage, and navigation", async () => {
    render(<DependencyInspector />);
    const item = await screen.findByTestId("deps-entity", {}, { timeout: 3000 });
    expect(item.getAttribute("data-entity-id")).toBe("thm");
    const families = within(item)
      .getAllByTestId("deps-family")
      .map((f) => f.getAttribute("data-family"));
    expect(families).toEqual(["math", "citation"]);
    expect(within(item).getByText("Mathematical dependencies")).toBeTruthy();
    expect(within(item).getByText("References")).toBeTruthy();
    expect(within(item).getByText("· observed")).toBeTruthy();
    expect(screen.getByTestId("deps-coverage").textContent).toMatch(/Partial \(latex\/1\)/);
    expect(screen.getByTestId("deps-resource").textContent).toMatch(/paper\.tex/);

    fireEvent.click(within(item).getByRole("button", { name: "Open in Mathematics" }));
    expect(useGraphPage.getState().section).toBe("mathematics");
    expect(useGraphNavigation.getState().request).toMatchObject({ section: "mathematics", entity: "thm" });
  });

  it("says when more items or relationships exist than it lists", async () => {
    const fake = createFakeGraph({
      entities: [
        entity("thm", "theorem", "Main theorem", { attributes: { path: "paper.tex" } }),
        entity("lem", "lemma", "Key lemma"),
        entity("doc", "resource", "paper.tex", { roles: ["manuscript"], key: "paper.tex" }),
      ],
      relations: [relation("d1", "dependsOn", "thm", "lem")],
    });
    const inspect = fake.api.inspectAtSource.bind(fake.api);
    setGraphApi({
      ...fake.api,
      inspectAtSource: async (request) => {
        const full = await inspect(request);
        return { ...full, entitiesTruncated: true, entities: full.entities.map((e) => ({ ...e, relationsTruncated: true })) };
      },
    });
    render(<DependencyInspector />);
    await screen.findByTestId("deps-entity", {}, { timeout: 3000 });
    expect(screen.getByTestId("deps-entities-truncated").textContent).toMatch(/first 1 items bound here; more exist/);
    expect(screen.getByTestId("deps-relations-truncated").textContent).toMatch(/first 1 relationships; this item has more/);
  });

  it("does not mention a limit when everything is listed", async () => {
    render(<DependencyInspector />);
    await screen.findByTestId("deps-entity", {}, { timeout: 3000 });
    expect(screen.queryByTestId("deps-entities-truncated")).toBeNull();
    expect(screen.queryByTestId("deps-relations-truncated")).toBeNull();
  });

  it("lists records a relationship cites as cited support under the item's evidence", async () => {
    // Review finding 1: the theorem has no evidence of its own; a relationship cites one record.
    const fake = createFakeGraph({
      entities: [entity("thm", "theorem", "Main theorem", { attributes: { path: "paper.tex" } }), entity("doc", "resource", "paper.tex", { roles: ["manuscript"], key: "paper.tex" })],
      relations: [],
    });
    const record: Evidence = {
      id: "ev-1",
      kind: "review",
      subject: null,
      operation: { kind: "agentTask", id: "task-1" },
      inputs: [],
      outcome: { kind: "succeeded" },
      environment: [],
      outputs: [],
      formal: null,
      recordedBy: { kind: "service", service: "agents" },
      recordedAtMs: 1,
    };
    setGraphApi({
      ...fake.api,
      entity: async (id) => ({ ...(await fake.api.entity(id)), citedEvidence: [record], evidenceFreshness: [[record.id, { kind: "unknown", reason: "no fingerprints" }]] }),
    });
    render(<DependencyInspector />);
    const cited = await screen.findByTestId("deps-cited-evidence", {}, { timeout: 3000 });
    expect(screen.getByTestId("deps-evidence").textContent).toBe("No evidence recorded for this item.");
    expect(screen.getByText("Cited by a relationship")).toBeTruthy();
    expect(screen.getByText(/These records are not recorded about it\./)).toBeTruthy();
    expect(within(cited).getAllByRole("listitem")).toHaveLength(1);
    expect(cited.textContent).toContain("Review");
    expect(within(cited).getByText("Unknown")).toBeTruthy();
  });

  it("says when nothing is bound at the caret", async () => {
    position.current = { path: "notes.md", offset: 0, line: 1, revision: 1, dirty: true, fromEditor: true };
    render(<DependencyInspector />);
    await waitFor(() => expect(screen.getByTestId("deps-none")).toBeTruthy(), { timeout: 3000 });
    expect(screen.getByText(/not a graph resource/)).toBeTruthy();
  });

  it("asks for a source file when none is active", () => {
    position.current = null;
    render(<DependencyInspector />);
    expect(screen.getByText("No source file active")).toBeTruthy();
  });
});
