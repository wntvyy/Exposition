import { describe, expect, it } from "vitest";

import { borderPoint, edgeGeometry, parallelOffset } from "./geometry";
import { layeredLayout, placeUnpositioned } from "./layout";
import { DEFAULT_NODE_SIZE } from "./types";

describe("edge geometry", () => {
  const a = { x: 0, y: 0, width: 200, height: 80 };
  const b = { x: 400, y: 0, width: 200, height: 80 };

  it("attaches to the facing borders and follows a moved node", () => {
    const g = edgeGeometry(a, b);
    expect(g.source).toEqual({ x: 200, y: 40 });
    expect(g.target).toEqual({ x: 400, y: 40 });
    // Move b below a: the edge now leaves a's bottom border and enters b's top.
    const moved = edgeGeometry(a, { ...b, x: 0, y: 300 });
    expect(moved.source).toEqual({ x: 100, y: 80 });
    expect(moved.target).toEqual({ x: 100, y: 300 });
    expect(moved.path).not.toEqual(g.path);
  });

  it("returns the center when the target is the center", () => {
    expect(borderPoint(a, { x: 100, y: 40 })).toEqual({ x: 100, y: 40 });
  });

  it("bows parallel edges apart on opposite sides", () => {
    expect(parallelOffset(0, 1)).toBe(0);
    expect(parallelOffset(0, 2)).toBe(-parallelOffset(1, 2));
    const first = edgeGeometry(a, b, parallelOffset(0, 2));
    const second = edgeGeometry(a, b, parallelOffset(1, 2));
    expect(first.label.y).toBeLessThan(40);
    expect(second.label.y).toBeGreaterThan(40);
    expect(first.path.startsWith("M ")).toBe(true);
    expect(first.path).toContain(" Q ");
  });
});

describe("layered layout", () => {
  const nodes = ["thm", "lemma", "def", "assumption", "lonely"].map((id) => ({ id }));
  const edges = [
    { source: "thm", target: "lemma" },
    { source: "lemma", target: "def" },
    { source: "thm", target: "assumption" },
  ];

  it("is deterministic and places dependencies in later layers", () => {
    const first = layeredLayout(nodes, edges);
    const second = layeredLayout([...nodes].reverse(), [...edges].reverse());
    expect([...first.entries()].sort()).toEqual([...second.entries()].sort());
    expect(first.get("thm")!.x).toBeLessThan(first.get("lemma")!.x);
    expect(first.get("lemma")!.x).toBeLessThan(first.get("def")!.x);
    expect(first.get("assumption")!.x).toBe(first.get("lemma")!.x);
    // The isolated node is stacked below the connected component.
    expect(first.get("lonely")!.y).toBeGreaterThan(Math.max(first.get("lemma")!.y, first.get("assumption")!.y));
  });

  it("never overlaps nodes in a layer", () => {
    const positions = layeredLayout(nodes, edges);
    const lemma = positions.get("lemma")!;
    const assumption = positions.get("assumption")!;
    expect(Math.abs(lemma.y - assumption.y)).toBeGreaterThanOrEqual(DEFAULT_NODE_SIZE.height);
  });

  it("terminates on cycles and ignores self-loops and unknown endpoints", () => {
    const positions = layeredLayout(
      [{ id: "a" }, { id: "b" }, { id: "c" }],
      [
        { source: "a", target: "b" },
        { source: "b", target: "c" },
        { source: "c", target: "a" },
        { source: "a", target: "a" },
        { source: "a", target: "missing" },
      ],
    );
    expect(positions.size).toBe(3);
    const xs = new Set([...positions.values()].map((p) => p.x));
    expect(xs.size).toBe(3);
  });

  it("aligns chains and centers a node between the nodes it relates to", () => {
    const chain = layeredLayout([{ id: "a" }, { id: "b" }, { id: "c" }], [
      { source: "a", target: "b" },
      { source: "b", target: "c" },
    ]);
    expect(new Set([...chain.values()].map((p) => p.y)).size).toBe(1);
    const fan = layeredLayout([{ id: "root" }, { id: "x" }, { id: "y" }], [
      { source: "root", target: "x" },
      { source: "root", target: "y" },
    ]);
    const mid = (fan.get("x")!.y + fan.get("y")!.y) / 2;
    expect(Math.abs(fan.get("root")!.y - mid)).toBeLessThanOrEqual(1);
  });

  it("keeps a minimum gap between nodes of a layer on a larger graph", () => {
    const ids = Array.from({ length: 24 }, (_, i) => `n${String(i).padStart(2, "0")}`);
    const edges = ids.flatMap((id, i) => [
      { source: id, target: ids[(i * 7 + 3) % ids.length]! },
      { source: id, target: ids[(i * 5 + 11) % ids.length]! },
    ]);
    const positions = layeredLayout(ids.map((id) => ({ id })), edges);
    const byColumn = new Map<number, number[]>();
    for (const p of positions.values()) (byColumn.get(p.x) ?? byColumn.set(p.x, []).get(p.x)!).push(p.y);
    for (const ys of byColumn.values()) {
      ys.sort((a, b) => a - b);
      for (let i = 1; i < ys.length; i++) expect(ys[i]! - ys[i - 1]!).toBeGreaterThanOrEqual(DEFAULT_NODE_SIZE.height);
    }
    expect([...layeredLayout(ids.map((id) => ({ id })), [...edges].reverse()).entries()].sort()).toEqual([...positions.entries()].sort());
  });

  it("lays out top to bottom on request", () => {
    const positions = layeredLayout(nodes, edges, { direction: "TB" });
    expect(positions.get("thm")!.y).toBeLessThan(positions.get("lemma")!.y);
  });

  it("places new nodes beside positioned ones without moving them", () => {
    const positioned = [
      { id: "thm", position: { x: 10, y: 20 } },
      { id: "lemma", position: { x: 500, y: 300 } },
    ];
    const placed = placeUnpositioned(positioned, [{ id: "def" }, { id: "assumption" }], edges);
    expect(placed.has("thm")).toBe(false);
    expect(placed.has("lemma")).toBe(false);
    for (const p of placed.values()) expect(p.x).toBeGreaterThan(500 + DEFAULT_NODE_SIZE.width);
  });
});
