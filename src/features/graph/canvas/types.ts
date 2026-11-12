// Canvas projection types: what the graph canvas draws. These are UI
// projections of the backend's typed model (entities, relations, saved view
// presentation); they carry display data only. Rust owns identities,
// semantics, admission and validation; the canvas never decides what a
// relation means.
import type { BadgeTone, IconComponent } from "../../../ui/components";
import type { GraphCategory } from "../../../ui/tokens";

/** Relation families of the closed vocabulary (display grouping and edge style). */
export type RelationFamily = "structure" | "resource" | "math" | "citation" | "evidence" | "formal" | "execution";

/**
 * Assertion status of a relation: an accepted assertion, an agent/user
 * suggestion not yet accepted, a relation observed by a parser (with coverage),
 * or a rejected suggestion (kept for provenance, normally hidden).
 */
export type AssertionStatus = "accepted" | "suggested" | "observed" | "rejected";

export interface CanvasBadge {
  label: string;
  tone: BadgeTone;
  dashed?: boolean;
  /** Longer explanation (tooltip and accessible description). */
  title?: string;
}

export interface Point {
  x: number;
  y: number;
}

export interface Size {
  width: number;
  height: number;
}

export interface CanvasNode {
  /** Entity id (opaque, from the backend). */
  id: string;
  title: string;
  /** Visible type indicator, e.g. "Theorem", "Folder". */
  typeLabel: string;
  typeIcon?: IconComponent;
  category: GraphCategory;
  badges: CanvasBadge[];
  /** Archived/inactive entity (dashed outline + badge). */
  inactive?: boolean;
  /** De-emphasized: not matching the current search. */
  dimmed?: boolean;
  /** Group node (folder, collection, research space) that can collapse its members. */
  group?: { collapsed: boolean; memberCount: number };
  position: Point;
  /** Saved size, if the view stores one. */
  size?: Size;
}

export interface CanvasEdge {
  /** Relation id (or a synthetic id for relations folded into a collapsed group). */
  id: string;
  source: string;
  target: string;
  /** Relation kind id, e.g. `depends_on`. */
  kind: string;
  /** Visible label, e.g. "depends on" (vocabulary direction text). */
  label: string;
  family: RelationFamily;
  status: AssertionStatus;
  /** Number of relations this edge stands for (> 1 when folded into a collapsed group). */
  count?: number;
  /** Accessible description: "Lemma 2 depends on Definition 1, suggested". */
  description: string;
}

export interface CanvasViewport {
  x: number;
  y: number;
  zoom: number;
}

/** Default node box used for layout before a node is measured. */
export const DEFAULT_NODE_SIZE: Size = { width: 208, height: 76 };
