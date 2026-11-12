// Custom React Flow edge: floats on node borders (stays attached and stretches
// as either node moves), bows apart from parallel relations, and ends in an
// arrowhead (filled for assertions, open for citations). Family sets the
// color and arrowhead; assertion status sets the dash pattern; the label
// names the relation and is always text.
import { BaseEdge, EdgeLabelRenderer, useInternalNode, type Edge, type EdgeProps, type InternalNode } from "@xyflow/react";
import { memo } from "react";

import { useCanvasContext } from "./context";
import { edgeGeometry, type Rect } from "./geometry";
import { DEFAULT_NODE_SIZE, type CanvasEdge } from "./types";

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

const STATUS_TEXT: Record<CanvasEdge["status"], string> = {
  accepted: "",
  suggested: "Suggested",
  observed: "Observed",
  rejected: "Rejected",
};

function RelationEdgeView({ id, source, target, data, selected }: EdgeProps<RelationRFEdge>) {
  const { markerPrefix, labels, selectedNodes, onSelectEdge } = useCanvasContext();
  const from = useInternalNode(source);
  const to = useInternalNode(target);
  if (!from || !to || !data) return null;
  const geometry = edgeGeometry(rectOf(from), rectOf(to), data.bow, data.forward);
  const marker = selected ? "selected" : data.family;
  const showLabel = labels === "all" || selected || selectedNodes.has(source) || selectedNodes.has(target);
  const status = STATUS_TEXT[data.status];
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
            aria-label={data.description}
          >
            {status && <span className="xp-graph-edge-label__status">{status}</span>}
            {data.label}
            {data.count !== undefined && data.count > 1 && <span className="xp-graph-edge-label__count">×{data.count}</span>}
          </button>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

export const RelationEdge = memo(RelationEdgeView);
