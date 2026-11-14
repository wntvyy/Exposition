import { describe, expect, it } from "vitest";

import { clearInterest, createInterestStore, edgeLabelVisible, RESTING_LABEL_FAMILIES, touches, type EdgeLabelInput } from "./interaction";
import type { RelationFamily } from "./types";

const base = (family: RelationFamily, patch: Partial<EdgeLabelInput> = {}): EdgeLabelInput => ({
  id: "r1",
  source: "thm",
  target: "lem",
  family,
  selected: false,
  labels: "all",
  selectedNodes: new Set(),
  interested: false,
  ...patch,
});

describe("edge label rule", () => {
  it("labels mathematical, reference, evidence, and formal relationships at rest", () => {
    for (const family of ["math", "citation", "evidence", "formal"] as const) {
      expect(RESTING_LABEL_FAMILIES.has(family)).toBe(true);
      expect(edgeLabelVisible(base(family))).toBe(true);
    }
  });

  it("keeps structure and resource relationships unlabelled at rest, even on small graphs", () => {
    for (const family of ["structure", "resource", "execution"] as const) {
      expect(RESTING_LABEL_FAMILIES.has(family)).toBe(false);
      expect(edgeLabelVisible(base(family, { labels: "all" }))).toBe(false);
    }
  });

  it("labels a structure relationship when it or one of its nodes is selected, hovered, or focused", () => {
    expect(edgeLabelVisible(base("structure", { selected: true }))).toBe(true);
    expect(edgeLabelVisible(base("structure", { selectedNodes: new Set(["lem"]) }))).toBe(true);
    expect(edgeLabelVisible(base("structure", { selectedNodes: new Set(["thm"]) }))).toBe(true);
    expect(edgeLabelVisible(base("structure", { selectedNodes: new Set(["other"]) }))).toBe(false);
    expect(edgeLabelVisible(base("structure", { interested: true }))).toBe(true);
  });

  it("above 60 edges labels only the selection and its neighbors (plus interest), whatever the family", () => {
    expect(edgeLabelVisible(base("math", { labels: "selection" }))).toBe(false);
    expect(edgeLabelVisible(base("math", { labels: "selection", selectedNodes: new Set(["thm"]) }))).toBe(true);
    expect(edgeLabelVisible(base("math", { labels: "selection", selected: true }))).toBe(true);
    expect(edgeLabelVisible(base("citation", { labels: "selection", interested: true }))).toBe(true);
  });

  it("an interest item touches an edge when it is the edge or one of its ends", () => {
    const edge = { id: "r1", source: "thm", target: "lem" };
    expect(touches(null, edge)).toBe(false);
    expect(touches({ kind: "edge", id: "r1" }, edge)).toBe(true);
    expect(touches({ kind: "edge", id: "r2" }, edge)).toBe(false);
    expect(touches({ kind: "node", id: "thm" }, edge)).toBe(true);
    expect(touches({ kind: "node", id: "lem" }, edge)).toBe(true);
    expect(touches({ kind: "node", id: "r1" }, edge)).toBe(false);
  });

  it("a late leave does not clear a newer hover", () => {
    const store = createInterestStore();
    store.setState({ hovered: { kind: "node", id: "b" } });
    clearInterest(store, "hovered", { kind: "node", id: "a" });
    expect(store.getState().hovered).toEqual({ kind: "node", id: "b" });
    clearInterest(store, "hovered", { kind: "node", id: "b" });
    expect(store.getState().hovered).toBeNull();
  });
});
