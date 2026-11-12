// Deterministic layered layout for the graph canvas. It runs only when the
// user asks ("Arrange") or to place nodes that have no saved position yet; it
// never moves positioned nodes on its own. Same input → same output: nodes and
// edges are processed in id order, ties break by id, and there is no
// randomness or simulation.
//
// Steps (a small Sugiyama-style pipeline): split into connected components;
// per component, break cycles by reversing DFS back edges; assign layers by
// longest path from the sources; order nodes within layers by a few
// barycenter sweeps; assign coordinates; stack components.
import { DEFAULT_NODE_SIZE, type Point, type Size } from "./types";

export type LayoutDirection = "LR" | "TB";

export interface LayoutNode {
  id: string;
  size?: Size;
}

export interface LayoutEdge {
  source: string;
  target: string;
}

export interface LayoutOptions {
  direction?: LayoutDirection;
  /** Gap between layers (px). */
  layerGap?: number;
  /** Gap between nodes within a layer (px). */
  nodeGap?: number;
  /** Gap between stacked components (px). */
  componentGap?: number;
  /** Top-left corner of the result. */
  origin?: Point;
}

const byId = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

function components(ids: string[], neighbors: Map<string, Set<string>>): string[][] {
  const seen = new Set<string>();
  const result: string[][] = [];
  for (const start of ids) {
    if (seen.has(start)) continue;
    const component: string[] = [];
    const stack = [start];
    seen.add(start);
    while (stack.length) {
      const id = stack.pop()!;
      component.push(id);
      for (const next of [...(neighbors.get(id) ?? [])].sort(byId)) {
        if (!seen.has(next)) {
          seen.add(next);
          stack.push(next);
        }
      }
    }
    result.push(component.sort(byId));
  }
  // Larger components first, then by smallest id.
  return result.sort((a, b) => b.length - a.length || byId(a[0]!, b[0]!));
}

/** Edges of an acyclic orientation: DFS back edges (in id order) are reversed. */
function acyclic(ids: string[], out: Map<string, string[]>): [string, string][] {
  const state = new Map<string, 1 | 2>(); // 1 = on stack, 2 = done
  const edges: [string, string][] = [];
  const visit = (root: string) => {
    const stack: { id: string; next: number }[] = [{ id: root, next: 0 }];
    state.set(root, 1);
    while (stack.length) {
      const frame = stack[stack.length - 1]!;
      const targets = out.get(frame.id) ?? [];
      if (frame.next >= targets.length) {
        state.set(frame.id, 2);
        stack.pop();
        continue;
      }
      const target = targets[frame.next++]!;
      const s = state.get(target);
      if (s === 1) edges.push([target, frame.id]); // back edge: reverse it
      else {
        edges.push([frame.id, target]);
        if (s === undefined) {
          state.set(target, 1);
          stack.push({ id: target, next: 0 });
        }
      }
    }
  };
  for (const id of ids) if (!state.has(id)) visit(id);
  return edges;
}

function layers(ids: string[], edges: [string, string][]): Map<string, number> {
  const indegree = new Map(ids.map((id) => [id, 0]));
  const out = new Map<string, string[]>(ids.map((id) => [id, []]));
  for (const [s, t] of edges) {
    out.get(s)!.push(t);
    indegree.set(t, indegree.get(t)! + 1);
  }
  const layer = new Map(ids.map((id) => [id, 0]));
  const queue = ids.filter((id) => indegree.get(id) === 0).sort(byId);
  while (queue.length) {
    const id = queue.shift()!;
    for (const t of out.get(id)!.sort(byId)) {
      layer.set(t, Math.max(layer.get(t)!, layer.get(id)! + 1));
      indegree.set(t, indegree.get(t)! - 1);
      if (indegree.get(t) === 0) {
        queue.push(t);
        queue.sort(byId);
      }
    }
  }
  return layer;
}

function order(rows: string[][], edges: [string, string][]): string[][] {
  const up = new Map<string, string[]>();
  const down = new Map<string, string[]>();
  for (const [s, t] of edges) {
    (down.get(s) ?? down.set(s, []).get(s)!).push(t);
    (up.get(t) ?? up.set(t, []).get(t)!).push(s);
  }
  const result = rows.map((row) => [...row]);
  const indexOf = new Map<string, number>();
  const reindex = () => result.forEach((row) => row.forEach((id, i) => indexOf.set(id, i)));
  reindex();
  const sweep = (rowIndex: number, neighbors: Map<string, string[]>) => {
    const row = result[rowIndex]!;
    const keyed = row.map((id, i) => {
      const ns = neighbors.get(id) ?? [];
      const bary = ns.length ? ns.reduce((sum, n) => sum + indexOf.get(n)!, 0) / ns.length : i;
      return { id, bary, i };
    });
    keyed.sort((a, b) => a.bary - b.bary || a.i - b.i || byId(a.id, b.id));
    result[rowIndex] = keyed.map((k) => k.id);
    reindex();
  };
  for (let pass = 0; pass < 4; pass++) {
    for (let r = 1; r < result.length; r++) sweep(r, up);
    for (let r = result.length - 2; r >= 0; r--) sweep(r, down);
  }
  return result;
}

/**
 * Layered positions (top-left corners) for every node. Edges whose endpoints
 * are not both in `nodes` are ignored, as are self-loops.
 */
export function layeredLayout(nodes: LayoutNode[], edges: LayoutEdge[], options: LayoutOptions = {}): Map<string, Point> {
  const direction = options.direction ?? "LR";
  const layerGap = options.layerGap ?? 96;
  const nodeGap = options.nodeGap ?? 28;
  const componentGap = options.componentGap ?? 72;
  const origin = options.origin ?? { x: 0, y: 0 };
  const size = new Map(nodes.map((n) => [n.id, n.size ?? DEFAULT_NODE_SIZE]));
  const ids = [...size.keys()].sort(byId);

  const out = new Map<string, string[]>(ids.map((id) => [id, []]));
  const neighbors = new Map<string, Set<string>>(ids.map((id) => [id, new Set()]));
  const seenEdges = new Set<string>();
  for (const e of [...edges].sort((a, b) => byId(a.source, b.source) || byId(a.target, b.target))) {
    if (e.source === e.target || !size.has(e.source) || !size.has(e.target)) continue;
    const key = `${e.source}\u0000${e.target}`;
    if (seenEdges.has(key)) continue;
    seenEdges.add(key);
    out.get(e.source)!.push(e.target);
    neighbors.get(e.source)!.add(e.target);
    neighbors.get(e.target)!.add(e.source);
  }

  const positions = new Map<string, Point>();
  // Offset along the stacking axis (y for LR, x for TB).
  let stackOffset = 0;
  for (const component of components(ids, neighbors)) {
    const compOut = new Map(component.map((id) => [id, out.get(id)!.filter((t) => component.includes(t))]));
    const dag = acyclic(component, compOut);
    const layerOf = layers(component, dag);
    const depth = Math.max(...component.map((id) => layerOf.get(id)!));
    const rows: string[][] = Array.from({ length: depth + 1 }, () => []);
    for (const id of component) rows[layerOf.get(id)!]!.push(id);
    const ordered = order(rows, dag);

    // Extent of each layer along the layer axis and across it.
    const along = (id: string) => (direction === "LR" ? size.get(id)!.width : size.get(id)!.height);
    const across = (id: string) => (direction === "LR" ? size.get(id)!.height : size.get(id)!.width);
    const layerThickness = ordered.map((row) => Math.max(0, ...row.map(along)));
    const layerLength = ordered.map((row) => row.reduce((sum, id) => sum + across(id), 0) + nodeGap * Math.max(0, row.length - 1));
    const longest = Math.max(...layerLength);

    let layerOffset = 0;
    ordered.forEach((row, r) => {
      let cursor = (longest - layerLength[r]!) / 2;
      for (const id of row) {
        const a = layerOffset + (layerThickness[r]! - along(id)) / 2;
        const b = stackOffset + cursor;
        positions.set(id, direction === "LR" ? { x: origin.x + a, y: origin.y + b } : { x: origin.x + b, y: origin.y + a });
        cursor += across(id) + nodeGap;
      }
      layerOffset += layerThickness[r]! + layerGap;
    });
    stackOffset += longest + componentGap;
  }
  return positions;
}

/**
 * Positions for nodes that have none yet, laid out among themselves and
 * placed to the right of (LR) or below (TB) the nodes that already have
 * positions. Positioned nodes are never moved.
 */
export function placeUnpositioned(
  positioned: { id: string; position: Point; size?: Size }[],
  unpositioned: LayoutNode[],
  edges: LayoutEdge[],
  options: LayoutOptions = {},
): Map<string, Point> {
  if (unpositioned.length === 0) return new Map();
  const direction = options.direction ?? "LR";
  if (positioned.length === 0) return layeredLayout(unpositioned, edges, options);
  let maxX = -Infinity;
  let maxY = -Infinity;
  let minX = Infinity;
  let minY = Infinity;
  for (const p of positioned) {
    const s = p.size ?? DEFAULT_NODE_SIZE;
    minX = Math.min(minX, p.position.x);
    minY = Math.min(minY, p.position.y);
    maxX = Math.max(maxX, p.position.x + s.width);
    maxY = Math.max(maxY, p.position.y + s.height);
  }
  const gap = options.componentGap ?? 120;
  const origin = direction === "LR" ? { x: maxX + gap, y: minY } : { x: minX, y: maxY + gap };
  return layeredLayout(unpositioned, edges, { ...options, origin });
}
