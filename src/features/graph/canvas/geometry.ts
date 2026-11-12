// Edge geometry for the canvas: edges float on node borders (they attach where
// the line between two nodes meets each rectangle), so connections stay
// attached and stretch as nodes move, whatever side the other node is on.
// Parallel relations between the same pair of nodes bow apart so each stays
// visible and clickable.
import type { Point, Size } from "./types";

export interface Rect extends Point, Size {}

export const center = (r: Rect): Point => ({ x: r.x + r.width / 2, y: r.y + r.height / 2 });

/**
 * The point where the ray from the rectangle's center toward `toward` leaves
 * the rectangle. Returns the center when `toward` is the center itself.
 */
export function borderPoint(rect: Rect, toward: Point): Point {
  const c = center(rect);
  const dx = toward.x - c.x;
  const dy = toward.y - c.y;
  if (dx === 0 && dy === 0) return c;
  const hw = rect.width / 2;
  const hh = rect.height / 2;
  const tx = dx === 0 ? Infinity : hw / Math.abs(dx);
  const ty = dy === 0 ? Infinity : hh / Math.abs(dy);
  const t = Math.min(tx, ty);
  return { x: c.x + dx * t, y: c.y + dy * t };
}

/** Spacing between parallel edges at their widest point (px). */
export const PARALLEL_SPACING = 22;

/**
 * Perpendicular bow (px) of edge `index` among `count` parallel edges between
 * the same two nodes. Signs are relative to the canonical direction (lower id
 * to higher id), so A→B and B→A edges bow to opposite sides.
 */
export function parallelOffset(index: number, count: number): number {
  if (count <= 1) return 0;
  return (index - (count - 1) / 2) * PARALLEL_SPACING;
}

export interface EdgeGeometry {
  /** SVG path data. */
  path: string;
  source: Point;
  target: Point;
  /** Point on the path halfway along it (label anchor). */
  label: Point;
}

/**
 * Geometry of an edge from `from` to `to`. `bow` bends the edge to one side
 * (positive: to the left of the canonical direction `canonicalFrom → canonicalTo`).
 */
export function edgeGeometry(from: Rect, to: Rect, bow = 0, canonicalForward = true): EdgeGeometry {
  const a = center(from);
  const b = center(to);
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const length = Math.hypot(dx, dy) || 1;
  // Unit normal of the canonical direction.
  const sign = canonicalForward ? 1 : -1;
  const nx = (-dy / length) * sign;
  const ny = (dx / length) * sign;
  const mid = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  if (bow === 0) {
    const source = borderPoint(from, b);
    const target = borderPoint(to, a);
    return {
      path: `M ${round(source.x)} ${round(source.y)} L ${round(target.x)} ${round(target.y)}`,
      source,
      target,
      label: { x: (source.x + target.x) / 2, y: (source.y + target.y) / 2 },
    };
  }
  // Quadratic curve whose apex sits `bow` px from the straight line.
  const control = { x: mid.x + nx * bow * 2, y: mid.y + ny * bow * 2 };
  const source = borderPoint(from, control);
  const target = borderPoint(to, control);
  const label = {
    x: 0.25 * source.x + 0.5 * control.x + 0.25 * target.x,
    y: 0.25 * source.y + 0.5 * control.y + 0.25 * target.y,
  };
  return {
    path: `M ${round(source.x)} ${round(source.y)} Q ${round(control.x)} ${round(control.y)} ${round(target.x)} ${round(target.y)}`,
    source,
    target,
    label,
  };
}

const round = (n: number) => Math.round(n * 10) / 10;

/** Bounding box of rectangles, or null when there are none. */
export function bounds(rects: Rect[]): Rect | null {
  if (rects.length === 0) return null;
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const r of rects) {
    minX = Math.min(minX, r.x);
    minY = Math.min(minY, r.y);
    maxX = Math.max(maxX, r.x + r.width);
    maxY = Math.max(maxY, r.y + r.height);
  }
  return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}
