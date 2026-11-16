// Custom React Flow edge: floats on node borders (stays attached and stretches
// as either node moves), bows apart from parallel relations, and ends in an
// arrowhead (filled for assertions, open for citations). Family sets the
// color and arrowhead; assertion status sets the dash pattern; the label
// names the relation and is always text. Which labels show follows
// `edgeLabelVisible` (content families at rest, structure on interest).
import { BaseEdge, EdgeLabelRenderer, useInternalNode, type Edge, type EdgeProps, type InternalNode } from "@xyflow/react";
import { memo } from "react";
import { useStore } from "zustand";

import { useCanvasContext } from "./context";
import { edgeGeometry, type Rect } from "./geometry";
import { clearInterest, edgeLabelVisible, touches } from "./interaction";
import { DEFAULT_NODE_SIZE, type AssertionStatus, type CanvasEdge } from "./types";

export interface RelationEdgeData extends Omit<CanvasEdge, "id" | "source" | "target"> {
  /** Perpendicular bow (px) when several relations join the same two nodes. */
  bow: number;
  /** True when source → target is the canonical (lower id → higher id) direction. */
  forward: boolean;
  [key: string]: unknown;
}

export type RelationRFEdge = Edge<RelationEdgeData, "relation">;

function rectOf(node: InternalNode): Rect {
  return {
    x: node.internals.positionAbsolute.x,
    y: node.internals.positionAbsolute.y,
    width: node.measured.width ?? DEFAULT_NODE_SIZE.width,
    height: node.measured.height ?? DEFAULT_NODE_SIZE.height,
  };
}

/** Status suffix after the reading ("depends on · suggested"); accepted assertions read plainly. */
const STATUS_SUFFIX: Record<AssertionStatus, string> = {
  accepted: "",
  suggested: "suggested",
  observed: "observed",
  rejected: "rejected",
};

/** The chip text: reading, folded count, then the muted status. */
export function EdgeLabelContent({ label, count, status, attention }: { label: string; count?: number; status: AssertionStatus; attention?: string }) {
  const suffix = attention ?? STATUS_SUFFIX[status];
  return (
    <>
      <span className="xp-graph-edge-label__reading">{label}</span>
      {count !== undefined && count > 1 && <span className="xp-graph-edge-label__count">×{count}</span>}
      {suffix && <span className="xp-graph-edge-label__status">· {suffix}</span>}
    </>
  );
}

function RelationEdgeView({ id, source, target, data, selected }: EdgeProps<RelationRFEdge>) {
  const { markerPrefix, labels, selectedNodes, interest, onSelectEdge } = useCanvasContext();
  const ends = { id, source, target };
  const interested = useStore(interest, (s) => touches(s.hovered, ends) || touches(s.focused, ends));
  const from = useInternalNode(source);
  const to = useInternalNode(target);
  if (!from || !to || !data) return null;
  const geometry = edgeGeometry(rectOf(from), rectOf(to), data.bow, data.forward);
  const marker = selected ? "selected" : data.family;
  const showLabel = edgeLabelVisible({ ...ends, family: data.family, selected: Boolean(selected), labels, selectedNodes, interested });
  const self = { kind: "edge" as const, id };
  return (
    <>
      <BaseEdge
        id={id}
        path={geometry.path}
        markerEnd={`url(#${markerPrefix}-${marker}${data.family === "citation" ? "-open" : ""})`}
        interactionWidth={14}
      />
      {showLabel && (
        <EdgeLabelRenderer>
          <button
            type="button"
            tabIndex={-1}
            className="xp-graph-edge-label nodrag nopan"
            data-edge-id={id}
            data-family={data.family}
            data-status={data.status}
            data-selected={selected || undefined}
            style={{ transform: `translate(-50%, -50%) translate(${geometry.label.x}px, ${geometry.label.y}px)` }}
            onClick={() => onSelectEdge(id)}
            onMouseEnter={() => interest.setState({ hovered: self })}
            onMouseLeave={() => clearInterest(interest, "hovered", self)}
            aria-label={data.description}
          >
            <EdgeLabelContent label={data.label} count={data.count} status={data.status} attention={data.attention} />
          </button>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

export const RelationEdge = memo(RelationEdgeView);
