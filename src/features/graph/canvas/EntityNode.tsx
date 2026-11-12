// Custom React Flow node: a freely movable colored rectangle (NodeCard) with
// a connection handle for typed relationship authoring. The whole node is a
// drop target while a connection is being drawn.
import { Handle, Position, useConnection, type Node, type NodeProps } from "@xyflow/react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { memo } from "react";

import { Badge, NodeCard } from "../../../ui/components";
import { useCanvasContext } from "./context";
import type { CanvasNode } from "./types";

export type EntityNodeData = Omit<CanvasNode, "position" | "size">;
export type EntityRFNode = Node<EntityNodeData, "entity">;

function EntityNodeView({ id, data, selected }: NodeProps<EntityRFNode>) {
  const { connectable, onToggleGroup } = useCanvasContext();
  const connecting = useConnection((c) => c.inProgress && c.fromNode.id !== id);
  const group = data.group;
  const toggle =
    group && onToggleGroup ? (
      <button
        type="button"
        className="xp-graph-node__toggle nodrag nopan"
        aria-label={`${group.collapsed ? "Expand" : "Collapse"} ${data.title}`}
        aria-expanded={!group.collapsed}
        onClick={(event) => {
          event.stopPropagation();
          onToggleGroup(id);
        }}
      >
        {group.collapsed ? <ChevronRight size={14} aria-hidden /> : <ChevronDown size={14} aria-hidden />}
      </button>
    ) : undefined;
  return (
    <div className="xp-graph-node" data-entity-id={id} data-category={data.category} data-connecting={connecting || undefined}>
      <NodeCard
        title={data.title}
        typeLabel={data.typeLabel}
        typeIcon={data.typeIcon}
        category={data.category}
        selected={selected}
        dimmed={data.dimmed}
        inactive={data.inactive}
        action={toggle}
        badges={
          data.badges.length > 0 || group?.collapsed ? (
            <>
              {group?.collapsed && (
                <Badge tone="neutral" title={`${group.memberCount} items hidden in this collapsed group`}>
                  {group.memberCount} inside
                </Badge>
              )}
              {data.badges.map((badge) => (
                <Badge key={badge.label} tone={badge.tone} dashed={badge.dashed} title={badge.title}>
                  {badge.label}
                </Badge>
              ))}
            </>
          ) : undefined
        }
      />
      {/* Drop target covering the node while a connection is drawn from another node. */}
      <Handle type="target" position={Position.Left} className="xp-graph-node__target" isConnectableStart={false} />
      <Handle
        type="source"
        position={Position.Right}
        className="xp-graph-node__source"
        isConnectable={connectable}
        isConnectableEnd={false}
        title={connectable ? `Drag to another item to relate ${data.title} to it` : undefined}
      />
    </div>
  );
}

export const EntityNode = memo(EntityNodeView);
