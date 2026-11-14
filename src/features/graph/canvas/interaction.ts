// Transient interest on the canvas (the hovered and the keyboard-focused node
// or edge) and the edge-label rule built on it. Interest lives in a small
// store so a hover re-renders only the edges whose label visibility changes,
// not every edge of a large graph.
import { createStore, type StoreApi } from "zustand";

import type { RelationFamily } from "./types";

export interface CanvasItemRef {
  kind: "node" | "edge";
  id: string;
}

export interface InterestState {
  hovered: CanvasItemRef | null;
  focused: CanvasItemRef | null;
}

export type InterestStore = StoreApi<InterestState>;

export const createInterestStore = (): InterestStore => createStore<InterestState>(() => ({ hovered: null, focused: null }));

/** Clear `key` only when it still points at `item` (a late leave must not clear a newer hover). */
export function clearInterest(store: InterestStore, key: keyof InterestState, item: CanvasItemRef): void {
  const current = store.getState()[key];
  if (current && current.kind === item.kind && current.id === item.id) store.setState({ [key]: null });
}

/**
 * Families whose labels carry the content of the graph (mathematical
 * dependencies, references, evidence, formal associations): shown at rest.
 * Structure and resource relationships (contains, is declared in, imports,
 * is a member of, …) are labelled only on interest, so they never bury the
 * mathematical readings.
 */
export const RESTING_LABEL_FAMILIES: ReadonlySet<RelationFamily> = new Set<RelationFamily>(["math", "citation", "evidence", "formal"]);

export interface EdgeEnds {
  id: string;
  source: string;
  target: string;
}

/** The interest item is this edge or one of its two nodes. */
export function touches(item: CanvasItemRef | null, edge: EdgeEnds): boolean {
  if (!item) return false;
  return item.kind === "edge" ? item.id === edge.id : item.id === edge.source || item.id === edge.target;
}

export interface EdgeLabelInput extends EdgeEnds {
  family: RelationFamily;
  /** The edge itself is selected. */
  selected: boolean;
  /** `all`: resting labels show (≤ 60 edges); `selection`: only near the selection and interest. */
  labels: "all" | "selection";
  selectedNodes: ReadonlySet<string>;
  /** The edge or one of its nodes is hovered or focused. */
  interested: boolean;
}

/**
 * Whether an edge shows its label:
 * - always when the edge is selected, touches a selected node, or is (or
 *   touches) the hovered/focused item;
 * - otherwise only for resting families, and only when the canvas shows
 *   resting labels (small graphs; above 60 edges only the selection and its
 *   neighbors are labelled).
 */
export function edgeLabelVisible(edge: EdgeLabelInput): boolean {
  if (edge.selected || edge.interested || edge.selectedNodes.has(edge.source) || edge.selectedNodes.has(edge.target)) return true;
  return edge.labels === "all" && RESTING_LABEL_FAMILIES.has(edge.family);
}
