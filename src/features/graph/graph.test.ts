import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GraphView } from "../../bindings/GraphView";
import { ipcGraphApi, setGraphApi } from "./api";
import { createFakeGraph, entity, fakeVocabulary, relation } from "./fakeApi";
import { defaultFilters, indexVocabulary, type SavedView } from "./model";
import { project, type ProjectionInput } from "./projection";
import { graph, resetGraphForTests, useGraph } from "./store";
import { createViewStore, emptyView, fromGraphView, toGraphViewInput } from "./viewStore";

const vocabulary = indexVocabulary(fakeVocabulary());

function input(): ProjectionInput {
  return {
    entities: [
      entity("root", "folder", "chapters"),
      entity("sub", "folder", "appendix"),
      entity("thm", "theorem", "Main theorem", { key: "thm:main" }),
      entity("lem", "lemma", "Key lemma"),
      entity("def", "definition", "Compactness", { lifecycle: "archived" }),
      entity("ref", "reference", "Rudin 1976"),
      entity("paper", "resource", "paper.tex", { roles: ["manuscript"] }),
    ],
    relations: [
      relation("c1", "contains", "root", "sub"),
      relation("c2", "contains", "root", "thm"),
      relation("c3", "contains", "sub", "lem"),
      relation("d1", "dependsOn", "thm", "lem"),
      relation("d2", "dependsOn", "lem", "def"),
      relation("x1", "references", "thm", "ref", "observed"),
      relation("x2", "references", "lem", "ref", "observed"),
      relation("s1", "dependsOn", "thm", "ref", "rejected"),
    ],
  };
}

const view = (patch: Partial<SavedView> = {}): SavedView => ({ ...emptyView("Project", { kind: "project" }), ...patch });

describe("projection", () => {
  it("keeps active entities, labels relations from the vocabulary, and hides rejected suggestions", () => {
    const p = project(input(), vocabulary, view());
    expect(p.nodes.map((n) => n.id).sort()).toEqual(["lem", "paper", "ref", "root", "sub", "thm"]);
    expect(p.filteredOut).toBe(1);
    const d1 = p.edges.find((e) => e.id === "d1")!;
    expect(d1).toMatchObject({ label: "depends on", family: "math", status: "accepted", description: "Main theorem depends on Key lemma" });
    expect(p.edges.find((e) => e.id === "x1")).toMatchObject({ family: "citation", status: "observed" });
    expect(p.edges.some((e) => e.id === "s1")).toBe(false);
    expect(p.edges.some((e) => e.id === "d2")).toBe(false); // its target is archived
  });

  it("labels resources by role and adds caller badges as text", () => {
    const p = project(input(), vocabulary, view(), (e) => (e.id === "lem" ? [{ label: "Stale", tone: "stale" }] : []));
    expect(p.nodes.find((n) => n.id === "paper")).toMatchObject({ typeLabel: "Manuscript", category: "document" });
    expect(p.nodes.find((n) => n.id === "thm")).toMatchObject({ typeLabel: "Theorem", category: "math" });
    expect(p.nodes.find((n) => n.id === "lem")!.badges.map((b) => b.label)).toEqual(["Stale"]);
  });

  it("shows inactive entities and rejected suggestions only on request", () => {
    const p = project(input(), vocabulary, view({ filters: { ...defaultFilters, showInactive: true } }));
    expect(p.nodes.find((n) => n.id === "def")).toMatchObject({ inactive: true, badges: [{ label: "Archived" }] });
    expect(p.edges.find((e) => e.id === "s1")).toMatchObject({ status: "rejected" });
  });

  it("hides entities from the view only and dims non-matching search results", () => {
    const p = project(input(), vocabulary, view({ hidden: ["ref"], filters: { ...defaultFilters, text: "lemma" } }));
    expect(p.hidden).toBe(1);
    expect(p.nodes.some((n) => n.id === "ref")).toBe(false);
    expect(p.nodes.find((n) => n.id === "lem")!.dimmed).toBe(false);
    expect(p.nodes.find((n) => n.id === "thm")!.dimmed).toBe(true);
    expect(p.matches).toBe(1);
  });

  it("folds a collapsed group's members into it and re-routes their relations", () => {
    const p = project(input(), vocabulary, view({ collapsed: ["sub"] }));
    expect(p.nodes.some((n) => n.id === "lem")).toBe(false);
    expect(p.nodes.find((n) => n.id === "sub")!.group).toEqual({ collapsed: true, memberCount: 1 });
    expect(p.folded).toBe(1);
    const folded = p.edges.find((e) => e.source === "thm" && e.target === "sub" && e.kind === "dependsOn")!;
    expect(folded.id).toMatch(/^folded:/);
    expect(folded.count).toBe(1);
    const outer = project(input(), vocabulary, view({ collapsed: ["root"] }));
    expect(outer.nodes.map((n) => n.id).sort()).toEqual(["paper", "ref", "root"]);
    const refs = outer.edges.find((e) => e.source === "root" && e.target === "ref" && e.kind === "references")!;
    expect(refs.count).toBe(2);
    expect(refs.description).toContain("(2 relationships)");
  });

  it("filters by entity and relation kinds", () => {
    const p = project(input(), vocabulary, view({ filters: { ...defaultFilters, kinds: ["theorem", "lemma"], relationKinds: ["dependsOn"] } }));
    expect(p.nodes.map((n) => n.id).sort()).toEqual(["lem", "thm"]);
    expect(p.edges.map((e) => e.id)).toEqual(["d1"]);
  });
});

describe("saved views", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    setGraphApi(ipcGraphApi);
  });

  it("round-trips the backend view shape", () => {
    const saved: GraphView = {
      id: "v1",
      name: "Project",
      scope: { kind: "project" },
      nodes: [{ entity: "a", x: 1, y: 2, width: null, height: 40 }],
      viewport: { x: 3, y: 4, zoom: 0.5 },
      filters: { kinds: ["theorem"], relationKinds: [], text: null, showInactive: true, depth: null },
      hidden: ["h"],
      collapsed: ["c"],
      revision: 4,
      updatedAtMs: 0,
    };
    const client = fromGraphView(saved);
    expect(client).toMatchObject({ id: "v1", revision: 4, positions: { a: { x: 1, y: 2, height: 40 } }, filters: { text: "", showInactive: true } });
    expect(toGraphViewInput(client)).toEqual({
      id: "v1",
      name: "Project",
      scope: { kind: "project" },
      nodes: [{ entity: "a", x: 1, y: 2, width: null, height: 40 }],
      viewport: { x: 3, y: 4, zoom: 0.5 },
      filters: { kinds: ["theorem"], relationKinds: [], text: null, showInactive: true, depth: null },
      hidden: ["h"],
      collapsed: ["c"],
      expectedRevision: 4,
    });
  });

  it("creates the view on first save, then saves moves and viewport with the expected revision", async () => {
    const fake = createFakeGraph();
    setGraphApi(fake.api);
    const store = createViewStore("Project", { kind: "project" }, { saveDelayMs: 100 });
    await store.load();
    store.moveNodes(new Map([["thm", { x: 10.4, y: 20.6 }]]));
    store.setViewport({ x: 5, y: 6, zoom: 1.25 });
    expect(store.use.getState().save).toBe("pending");
    expect(fake.calls.filter((c) => c.method === "viewSave")).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(150);
    expect(fake.calls.filter((c) => c.method === "viewSave")).toHaveLength(1);
    const [saved] = [...fake.views.values()];
    expect(saved).toMatchObject({ nodes: [{ entity: "thm", x: 10, y: 21 }], viewport: { x: 5, y: 6, zoom: 1.25 }, revision: 1 });
    expect(store.use.getState()).toMatchObject({ save: "saved", view: { id: saved!.id, revision: 1 } });

    store.moveNodes(new Map([["thm", { x: 50, y: 60 }]]));
    await vi.advanceTimersByTimeAsync(150);
    expect(fake.views.get(saved!.id)).toMatchObject({ nodes: [{ entity: "thm", x: 50, y: 60 }], revision: 2 });

    // Reopening (e.g. after a reload) restores the layout of the project-scope view.
    const reopened = createViewStore("Project", { kind: "project" });
    await reopened.load();
    expect(reopened.use.getState().view).toMatchObject({ positions: { thm: { x: 50, y: 60 } }, viewport: { zoom: 1.25 } });
  });

  it("never moves an already positioned node when placing new ones", async () => {
    const fake = createFakeGraph();
    setGraphApi(fake.api);
    const store = createViewStore("Project", { kind: "project" }, { saveDelayMs: 10 });
    await store.load();
    store.moveNodes(new Map([["a", { x: 1, y: 1 }]]));
    store.placeNodes(new Map([["a", { x: 500, y: 500 }], ["b", { x: 300, y: 0 }]]));
    expect(store.use.getState().view!.positions).toEqual({ a: { x: 1, y: 1 }, b: { x: 300, y: 0 } });
    store.placeNodes(new Map([["a", { x: 500, y: 500 }]]), { replace: true });
    expect(store.use.getState().view!.positions.a).toEqual({ x: 500, y: 500 });
  });

  it("adopts the newer saved view when the save is stale", async () => {
    const fake = createFakeGraph();
    setGraphApi(fake.api);
    const base = toGraphViewInput(view({ positions: { thm: { x: 99, y: 99 } } }));
    const created = await fake.api.viewSave(base);
    const store = createViewStore("Project", { kind: "project" }, { saveDelayMs: 10 });
    await store.load();
    // Someone else saves the view meanwhile.
    await fake.api.viewSave({ ...base, id: created.id, expectedRevision: created.revision, nodes: [{ entity: "thm", x: 1, y: 2, width: null, height: null }] });
    store.moveNodes(new Map([["thm", { x: 50, y: 50 }]]));
    await vi.advanceTimersByTimeAsync(20);
    expect(store.use.getState().view).toMatchObject({ revision: 2, positions: { thm: { x: 1, y: 2 } } });
  });
});

describe("graph store", () => {
  afterEach(() => {
    resetGraphForTests();
    setGraphApi(ipcGraphApi);
  });

  it("does nothing until started, then follows events and re-reads on resync", async () => {
    const fake = createFakeGraph();
    setGraphApi(fake.api);
    expect(fake.calls).toHaveLength(0);
    graph.start();
    await vi.waitFor(() => expect(useGraph.getState().status?.revision).toBe(1));
    expect(useGraph.getState().vocabulary?.version).toBe(1);
    fake.emit({
      kind: "changed",
      delta: { revision: 5, entitiesAdded: [], entitiesUpdated: [], entitiesRemoved: [], relationsAdded: [], relationsUpdated: [], relationsRemoved: [], truncated: false },
      origin: { kind: "user" },
    });
    expect(useGraph.getState().revision).toBe(5);
    const epoch = useGraph.getState().epoch;
    fake.resync();
    expect(useGraph.getState().epoch).toBe(epoch + 1);
    await vi.waitFor(() => expect(fake.calls.filter((c) => c.method === "status")).toHaveLength(2));
  });

  it("reports an unavailable service instead of pretending", async () => {
    setGraphApi({
      ...createFakeGraph().api,
      status: () => Promise.reject({ code: "graph.unavailable", message: "The graph is not available" }),
    });
    graph.start();
    await vi.waitFor(() => expect(useGraph.getState().statusError).toBe("The graph is not available"));
    expect(useGraph.getState().status).toBeNull();
  });
});
