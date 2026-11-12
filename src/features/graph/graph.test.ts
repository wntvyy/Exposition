import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setGraphApi, unavailableGraphApi } from "./api";
import { createFakeGraph, entity, fakeVocabulary, relation } from "./fakeApi";
import { defaultFilters, type SavedView, type Snapshot } from "./model";
import { project } from "./projection";
import { graph, resetGraphForTests, useGraph } from "./store";
import { createViewStore, emptyView } from "./viewStore";

const vocabulary = fakeVocabulary();

function snapshot(): Snapshot {
  return {
    revision: 3,
    entities: [
      entity("root", "folder", "chapters"),
      entity("sub", "folder", "appendix"),
      entity("thm", "theorem", "Main theorem", { path: "chapters/main.tex", stale: true }),
      entity("lem", "lemma", "Key lemma", { path: "appendix/a.tex", binding: "missing" }),
      entity("def", "definition", "Compactness", { lifecycle: "archived" }),
      entity("ref", "reference", "Rudin 1976"),
    ],
    relations: [
      relation("c1", "contains", "root", "sub"),
      relation("c2", "contains", "root", "thm"),
      relation("c3", "contains", "sub", "lem"),
      relation("d1", "depends_on", "thm", "lem"),
      relation("d2", "depends_on", "lem", "def"),
      relation("x1", "references", "thm", "ref", "observed"),
      relation("x2", "references", "lem", "ref", "observed"),
      relation("s1", "depends_on", "thm", "ref", "rejected"),
    ],
    truncated: false,
    total: 6,
    coverage: { state: "partial", unresolved: 0 },
    freshness: { kind: "synchronized" },
  };
}

const view = (patch: Partial<SavedView> = {}): SavedView => ({ ...emptyView("project", "Project"), ...patch });

describe("projection", () => {
  it("keeps active entities, labels relations from the vocabulary, and hides rejected suggestions", () => {
    const p = project(snapshot(), vocabulary, view());
    expect(p.nodes.map((n) => n.id).sort()).toEqual(["lem", "ref", "root", "sub", "thm"]);
    expect(p.filteredOut).toBe(1);
    const d1 = p.edges.find((e) => e.id === "d1")!;
    expect(d1).toMatchObject({ label: "depends on", family: "math", status: "accepted", description: "Main theorem depends on Key lemma" });
    expect(p.edges.find((e) => e.id === "x1")).toMatchObject({ family: "citation", status: "observed" });
    expect(p.edges.some((e) => e.id === "s1")).toBe(false);
    expect(p.edges.some((e) => e.id === "d2")).toBe(false); // its target is archived
  });

  it("shows binding and freshness problems as text badges", () => {
    const p = project(snapshot(), vocabulary, view());
    expect(p.nodes.find((n) => n.id === "thm")!.badges.map((b) => b.label)).toEqual(["Stale"]);
    expect(p.nodes.find((n) => n.id === "lem")!.badges.map((b) => b.label)).toEqual(["Source missing"]);
  });

  it("hides entities from the view only and dims non-matching search results", () => {
    const p = project(snapshot(), vocabulary, view({ hidden: ["ref"], filters: { ...defaultFilters, text: "lemma" } }));
    expect(p.hidden).toBe(1);
    expect(p.nodes.some((n) => n.id === "ref")).toBe(false);
    expect(p.nodes.find((n) => n.id === "lem")!.dimmed).toBe(false);
    expect(p.nodes.find((n) => n.id === "thm")!.dimmed).toBe(true);
    expect(p.matches).toBe(1);
  });

  it("folds a collapsed group's members into it and re-routes their relations", () => {
    const p = project(snapshot(), vocabulary, view({ collapsed: ["sub"] }));
    expect(p.nodes.some((n) => n.id === "lem")).toBe(false);
    expect(p.nodes.find((n) => n.id === "sub")!.group).toEqual({ collapsed: true, memberCount: 1 });
    expect(p.folded).toBe(1);
    const folded = p.edges.find((e) => e.source === "thm" && e.target === "sub" && e.kind === "depends_on")!;
    expect(folded.id).toMatch(/^folded:/);
    expect(folded.count).toBe(1);
    // Collapsing the outer group folds everything inside it; relations to outside entities aggregate.
    const outer = project(snapshot(), vocabulary, view({ collapsed: ["root"] }));
    expect(outer.nodes.map((n) => n.id).sort()).toEqual(["ref", "root"]);
    const refs = outer.edges.find((e) => e.source === "root" && e.target === "ref" && e.kind === "references")!;
    expect(refs.count).toBe(2);
    expect(refs.description).toContain("(2 relationships)");
  });

  it("filters by entity and relation kinds", () => {
    const p = project(snapshot(), vocabulary, view({ filters: { ...defaultFilters, entityKinds: ["theorem", "lemma"], relationKinds: ["depends_on"] } }));
    expect(p.nodes.map((n) => n.id).sort()).toEqual(["lem", "thm"]);
    expect(p.edges.map((e) => e.id)).toEqual(["d1"]);
  });
});

describe("saved view store", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    setGraphApi(unavailableGraphApi);
  });

  it("saves moves and viewport after a pause with the expected revision", async () => {
    const fake = createFakeGraph();
    setGraphApi(fake.api);
    const store = createViewStore("project", "Project", { saveDelayMs: 100 });
    await store.load();
    store.moveNodes(new Map([["thm", { x: 10.4, y: 20.6 }]]));
    store.setViewport({ x: 5, y: 6, zoom: 1.25 });
    expect(store.use.getState().save).toBe("pending");
    expect(fake.calls.filter((c) => c.method === "viewSave")).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(150);
    const saves = fake.calls.filter((c) => c.method === "viewSave");
    expect(saves).toHaveLength(1);
    expect(fake.views.get("project")).toMatchObject({ positions: { thm: { x: 10, y: 21 } }, viewport: { x: 5, y: 6, zoom: 1.25 }, revision: 1 });
    expect(store.use.getState()).toMatchObject({ save: "saved", view: { revision: 1 } });

    // A second store (e.g. after a reload) restores the saved layout.
    const reopened = createViewStore("project", "Project");
    await reopened.load();
    expect(reopened.use.getState().view).toMatchObject({ positions: { thm: { x: 10, y: 21 } }, viewport: { zoom: 1.25 } });
  });

  it("never moves an already positioned node when placing new ones", async () => {
    const fake = createFakeGraph();
    setGraphApi(fake.api);
    const store = createViewStore("project", "Project", { saveDelayMs: 10 });
    await store.load();
    store.moveNodes(new Map([["a", { x: 1, y: 1 }]]));
    store.placeNodes(new Map([["a", { x: 500, y: 500 }], ["b", { x: 300, y: 0 }]]));
    expect(store.use.getState().view!.positions).toEqual({ a: { x: 1, y: 1 }, b: { x: 300, y: 0 } });
    store.placeNodes(new Map([["a", { x: 500, y: 500 }]]), { replace: true });
    expect(store.use.getState().view!.positions.a).toEqual({ x: 500, y: 500 });
  });

  it("adopts the newer saved view on a conflict", async () => {
    const fake = createFakeGraph();
    setGraphApi(fake.api);
    fake.views.set("project", { ...view(), revision: 7, positions: { thm: { x: 99, y: 99 } } });
    const store = createViewStore("project", "Project", { saveDelayMs: 10 });
    await store.load();
    fake.views.set("project", { ...view(), revision: 8, positions: { thm: { x: 1, y: 2 } } });
    store.moveNodes(new Map([["thm", { x: 50, y: 50 }]]));
    await vi.advanceTimersByTimeAsync(20);
    expect(store.use.getState().view).toMatchObject({ revision: 8, positions: { thm: { x: 1, y: 2 } } });
  });
});

describe("graph store", () => {
  afterEach(() => {
    resetGraphForTests();
    setGraphApi(unavailableGraphApi);
  });

  it("does nothing until started, then follows events and re-reads on resync", async () => {
    const fake = createFakeGraph();
    setGraphApi(fake.api);
    expect(fake.calls).toHaveLength(0);
    graph.start();
    await vi.waitFor(() => expect(useGraph.getState().status?.revision).toBe(1));
    expect(useGraph.getState().vocabulary?.version).toBe("test-1");
    fake.emit({ kind: "changed", revision: 5, entities: [], relations: [] });
    expect(useGraph.getState().revision).toBe(5);
    const epoch = useGraph.getState().epoch;
    fake.resync();
    expect(useGraph.getState().epoch).toBe(epoch + 1);
    await vi.waitFor(() => expect(fake.calls.filter((c) => c.method === "status")).toHaveLength(2));
  });

  it("reports an unavailable service instead of pretending", async () => {
    graph.start();
    await vi.waitFor(() => expect(useGraph.getState().statusError).toMatch(/not available/));
    expect(useGraph.getState().status).toBeNull();
  });
});
