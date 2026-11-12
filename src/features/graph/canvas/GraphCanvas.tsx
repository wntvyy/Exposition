// The graph canvas: a React Flow projection of typed entities and relations.
//
// Interaction standard (UI_STANDARD.md "Canvas"):
// - Pan: drag the background, or Space/middle-button drag anywhere.
// - Zoom: mouse wheel / trackpad pinch, the zoom buttons, `+`/`-`.
// - Fit view (Shift+1), fit selection (Shift+2), reset to 100% (`0`).
// - Select: click; Shift+drag draws a selection box; Ctrl/Cmd+click adds.
// - Move: drag a node, or arrow keys on a focused/selected node. Moving changes
//   layout only; the parent persists positions in the saved view.
// - Connect: drag from a node's handle onto another node. The canvas never
//   creates a relation itself: it reports the request (`onConnect`) and the
//   page proposes a typed change to the backend.
// - Nothing on the canvas deletes anything (the Delete key is not bound).
//
// The component is controlled through plain props (nodes, edges, viewport,
// selection) and knows nothing about IPC or binding shapes.
import "@xyflow/react/dist/base.css";
import "./canvas.css";

import {
  applyNodeChanges,
  ConnectionMode,
  Panel,
  ReactFlow,
  ReactFlowProvider,
  useNodesInitialized,
  useReactFlow,
  useViewport,
  type EdgeChange,
  type NodeChange,
  type OnConnect,
  type Viewport,
} from "@xyflow/react";
import { Focus, Maximize, Minus, Plus, ScanSearch } from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";

import { IconButton, Toolbar } from "../../../ui/components";
import { CanvasContext, type CanvasContextValue } from "./context";
import { EntityNode, type EntityRFNode } from "./EntityNode";
import { parallelOffset } from "./geometry";
import { RelationEdge, type RelationRFEdge } from "./RelationEdge";
import type { CanvasEdge, CanvasNode, CanvasViewport, Point, RelationFamily } from "./types";

export interface CanvasSelection {
  nodes: string[];
  edge: string | null;
}

export interface GraphCanvasProps {
  /** Accessible name of the canvas, e.g. "Project graph". */
  "aria-label": string;
  nodes: CanvasNode[];
  edges: CanvasEdge[];
  /** Saved viewport; `null` fits the content once it is measured. */
  viewport: CanvasViewport | null;
  selection: CanvasSelection;
  labels?: "all" | "selection";
  onSelectionChange: (selection: CanvasSelection) => void;
  /** Committed moves (drag end, keyboard move): new top-left positions. */
  onNodesMoved: (moves: Map<string, Point>) => void;
  /** The viewport after a pan/zoom ends. */
  onViewportChange: (viewport: CanvasViewport) => void;
  /** Present when the user may draw typed relationships between nodes. */
  onConnect?: (source: string, target: string) => void;
  onToggleGroup?: (id: string) => void;
  /** Double click on a node. */
  onActivateNode?: (id: string) => void;
  /** Bump `nonce` to fit the view to `nodes` (all nodes when absent), e.g. after Arrange or navigation. */
  fitRequest?: { nonce: number; nodes?: string[] };
  /** Extra controls appended to the canvas toolbar. */
  controls?: ReactNode;
  /** Overlay shown when there are no nodes. */
  empty?: ReactNode;
  className?: string;
  "data-testid"?: string;
}

const nodeTypes = { entity: EntityNode };
const edgeTypes = { relation: RelationEdge };
const FAMILIES: RelationFamily[] = ["structure", "resource", "math", "citation", "evidence", "formal", "execution"];
const reducedMotion = () => globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
const duration = () => (reducedMotion() ? 0 : 160);

/** React Flow node for a canvas node, keeping measured size and position of `previous`. */
export function toFlowNode(node: CanvasNode, previous: EntityRFNode | undefined, selected: boolean): EntityRFNode {
  const { position, size, ...data } = node;
  const ariaLabel = [node.typeLabel, node.title, ...node.badges.map((b) => b.label), node.inactive ? "archived" : ""]
    .filter(Boolean)
    .join(", ");
  return {
    id: node.id,
    type: "entity",
    position: previous?.dragging ? previous.position : position,
    data,
    selected,
    ariaLabel,
    ...(size ? { width: size.width, height: size.height } : {}),
    ...(previous?.measured ? { measured: previous.measured } : {}),
    ...(previous?.dragging ? { dragging: true } : {}),
  };
}

/** React Flow edges for canvas edges: parallel relations between one pair of nodes get distinct bows. */
export function toFlowEdges(edges: CanvasEdge[], selectedEdge: string | null): RelationRFEdge[] {
  const groups = new Map<string, CanvasEdge[]>();
  for (const edge of edges) {
    const key = edge.source < edge.target ? `${edge.source}\u0000${edge.target}` : `${edge.target}\u0000${edge.source}`;
    (groups.get(key) ?? groups.set(key, []).get(key)!).push(edge);
  }
  const bows = new Map<string, number>();
  for (const group of groups.values()) {
    group.sort((a, b) => (a.id < b.id ? -1 : 1)).forEach((edge, i) => bows.set(edge.id, parallelOffset(i, group.length)));
  }
  return edges.map((edge) => ({
    id: edge.id,
    type: "relation",
    source: edge.source,
    target: edge.target,
    selected: edge.id === selectedEdge,
    ariaLabel: edge.description,
    className: `xp-edge xp-edge--${edge.family} xp-edge--${edge.status}`,
    data: {
      kind: edge.kind,
      label: edge.label,
      family: edge.family,
      status: edge.status,
      count: edge.count,
      description: edge.description,
      bow: bows.get(edge.id) ?? 0,
      forward: edge.source < edge.target,
    },
  }));
}

function Markers({ prefix }: { prefix: string }) {
  const closed = (id: string, family: string) => (
    <marker
      key={id}
      id={id}
      viewBox="0 0 10 10"
      refX="9.5"
      refY="5"
      markerWidth="9"
      markerHeight="9"
      markerUnits="userSpaceOnUse"
      orient="auto-start-reverse"
    >
      <path d="M 0 0.5 L 10 5 L 0 9.5 z" className="xp-graph-marker" data-family={family} />
    </marker>
  );
  const open = (id: string, family: string) => (
    <marker
      key={id}
      id={id}
      viewBox="0 0 10 10"
      refX="9"
      refY="5"
      markerWidth="10"
      markerHeight="10"
      markerUnits="userSpaceOnUse"
      orient="auto-start-reverse"
    >
      <path d="M 1 1 L 9 5 L 1 9" className="xp-graph-marker xp-graph-marker--open" data-family={family} />
    </marker>
  );
  return (
    <svg className="xp-graph-markers" aria-hidden width="0" height="0">
      <defs>
        {FAMILIES.map((family) => closed(`${prefix}-${family}`, family))}
        {open(`${prefix}-citation-open`, "citation")}
        {closed(`${prefix}-selected`, "selected")}
        {open(`${prefix}-selected-open`, "selected")}
      </defs>
    </svg>
  );
}

function CanvasControls({ selection, extra }: { selection: string[]; extra?: ReactNode }) {
  const flow = useReactFlow();
  const { zoom } = useViewport();
  return (
    <Panel position="top-right" className="xp-graph-controls">
      <Toolbar aria-label="Canvas view controls" className="xp-graph-controls__toolbar">
        <IconButton icon={Minus} size="sm" aria-label="Zoom out" keybinding="-" onPress={() => void flow.zoomOut({ duration: duration() })} />
        <span className="xp-graph-controls__zoom" aria-live="off" data-testid="canvas-zoom">
          {Math.round(zoom * 100)}%
        </span>
        <IconButton icon={Plus} size="sm" aria-label="Zoom in" keybinding="+" onPress={() => void flow.zoomIn({ duration: duration() })} />
        <IconButton
          icon={Maximize}
          size="sm"
          aria-label="Fit to view"
          keybinding="Shift+1"
          onPress={() => void flow.fitView({ padding: 0.15, duration: duration() })}
        />
        <IconButton
          icon={ScanSearch}
          size="sm"
          aria-label="Fit to selection"
          keybinding="Shift+2"
          isDisabled={selection.length === 0}
          onPress={() => void flow.fitView({ nodes: selection.map((id) => ({ id })), padding: 0.3, duration: duration(), maxZoom: 1.5 })}
        />
        <IconButton
          icon={Focus}
          size="sm"
          aria-label="Reset view (100%)"
          keybinding="0"
          onPress={() => void flow.setViewport({ x: 0, y: 0, zoom: 1 }, { duration: duration() })}
        />
        {extra}
      </Toolbar>
    </Panel>
  );
}

const sameViewport = (a: Viewport | null, b: Viewport | null) =>
  a !== null && b !== null && Math.abs(a.x - b.x) < 0.5 && Math.abs(a.y - b.y) < 0.5 && Math.abs(a.zoom - b.zoom) < 0.001;

function CanvasInner(props: GraphCanvasProps) {
  const {
    nodes,
    edges,
    viewport,
    selection,
    labels = "selection",
    onSelectionChange,
    onNodesMoved,
    onViewportChange,
    onConnect,
    onToggleGroup,
    onActivateNode,
  } = props;
  const flow = useReactFlow();
  const rawId = useId();
  const markerPrefix = `xpg${rawId.replace(/[^a-zA-Z0-9]/g, "")}`;

  // Selection mirrored in a ref so node and edge change batches in one event combine.
  const selectionRef = useRef(selection);
  selectionRef.current = selection;
  const pendingSelection = useRef<CanvasSelection | null>(null);
  const emitSelection = useCallback(
    (next: CanvasSelection) => {
      const first = pendingSelection.current === null;
      pendingSelection.current = next;
      if (!first) return;
      queueMicrotask(() => {
        const value = pendingSelection.current!;
        pendingSelection.current = null;
        const current = selectionRef.current;
        const same =
          value.edge === current.edge &&
          value.nodes.length === current.nodes.length &&
          value.nodes.every((id) => current.nodes.includes(id));
        if (!same) onSelectionChange(value);
      });
    },
    [onSelectionChange],
  );

  const selectedNodes = useMemo(() => new Set(selection.nodes), [selection.nodes]);
  const [flowNodes, setFlowNodes] = useState<EntityRFNode[]>(() => nodes.map((n) => toFlowNode(n, undefined, selectedNodes.has(n.id))));
  useEffect(() => {
    setFlowNodes((previous) => {
      const byId = new Map(previous.map((n) => [n.id, n]));
      return nodes.map((n) => toFlowNode(n, byId.get(n.id), selectedNodes.has(n.id)));
    });
  }, [nodes, selectedNodes]);
  const flowEdges = useMemo(() => toFlowEdges(edges, selection.edge), [edges, selection.edge]);

  const onNodesChange = useCallback(
    (changes: NodeChange<EntityRFNode>[]) => {
      setFlowNodes((current) => applyNodeChanges(changes, current));
      const moves = new Map<string, Point>();
      let nodesSelection: Set<string> | null = null;
      for (const change of changes) {
        if (change.type === "position" && change.dragging === false && change.position) {
          moves.set(change.id, { x: change.position.x, y: change.position.y });
        }
        if (change.type === "select") {
          nodesSelection ??= new Set(pendingSelection.current?.nodes ?? selectionRef.current.nodes);
          if (change.selected) nodesSelection.add(change.id);
          else nodesSelection.delete(change.id);
        }
      }
      if (moves.size) onNodesMoved(moves);
      if (nodesSelection) {
        const base = pendingSelection.current ?? selectionRef.current;
        emitSelection({ nodes: [...nodesSelection], edge: nodesSelection.size ? null : base.edge });
      }
    },
    [emitSelection, onNodesMoved],
  );

  const onEdgesChange = useCallback(
    (changes: EdgeChange<RelationRFEdge>[]) => {
      let edge: string | null | undefined;
      const base = pendingSelection.current ?? selectionRef.current;
      for (const change of changes) {
        if (change.type !== "select") continue;
        if (change.selected) edge = change.id;
        else if (edge === undefined && base.edge === change.id) edge = null;
      }
      if (edge === undefined) return;
      emitSelection({ nodes: edge ? [] : base.nodes, edge });
    },
    [emitSelection],
  );

  const handleConnect: OnConnect = useCallback(
    (connection) => {
      if (connection.source && connection.target && connection.source !== connection.target) {
        onConnect?.(connection.source, connection.target);
      }
    },
    [onConnect],
  );

  // Viewport: restore the saved one, fit when there is none, report pans/zooms.
  const lastViewport = useRef<Viewport | null>(viewport);
  const initialized = useNodesInitialized();
  const fitted = useRef(false);
  useEffect(() => {
    if (viewport && !sameViewport(viewport, lastViewport.current)) {
      lastViewport.current = viewport;
      void flow.setViewport(viewport);
    }
  }, [viewport, flow]);
  const fitNonce = props.fitRequest?.nonce ?? 0;
  const fitNodes = props.fitRequest?.nodes;
  useEffect(() => {
    if (!fitNonce) return;
    // Two frames: let new positions/nodes reach React Flow's store first.
    let second = 0;
    const first = requestAnimationFrame(() => {
      second = requestAnimationFrame(() => {
        void flow.fitView({
          padding: fitNodes ? 0.4 : 0.15,
          maxZoom: 1,
          duration: duration(),
          ...(fitNodes ? { nodes: fitNodes.map((id) => ({ id })) } : {}),
        });
      });
    });
    return () => {
      cancelAnimationFrame(first);
      cancelAnimationFrame(second);
    };
    // `fitNodes` is read when the nonce changes.
  }, [fitNonce, flow]);
  useEffect(() => {
    if (!viewport && initialized && !fitted.current && nodes.length > 0) {
      fitted.current = true;
      void flow.fitView({ padding: 0.15, maxZoom: 1 });
    }
  }, [viewport, initialized, nodes.length, flow]);

  const context: CanvasContextValue = useMemo(
    () => ({
      markerPrefix,
      connectable: Boolean(onConnect),
      labels,
      selectedNodes,
      onToggleGroup,
      onSelectEdge: (id: string) => emitSelection({ nodes: [], edge: id }),
    }),
    [markerPrefix, onConnect, labels, selectedNodes, onToggleGroup, emitSelection],
  );

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const target = event.target as HTMLElement;
    if (target.closest("input, textarea, [contenteditable]") || event.ctrlKey || event.metaKey || event.altKey) return;
    const d = duration();
    if (event.key === "+" || event.key === "=") void flow.zoomIn({ duration: d });
    else if (event.key === "-" || event.key === "_") void flow.zoomOut({ duration: d });
    else if (event.shiftKey && event.code === "Digit1") void flow.fitView({ padding: 0.15, duration: d });
    else if (event.shiftKey && event.code === "Digit2" && selection.nodes.length) {
      void flow.fitView({ nodes: selection.nodes.map((id) => ({ id })), padding: 0.3, duration: d, maxZoom: 1.5 });
    } else if (event.key === "0" && !event.shiftKey) void flow.setViewport({ x: 0, y: 0, zoom: 1 }, { duration: d });
    else return;
    event.preventDefault();
  };

  return (
    <CanvasContext.Provider value={context}>
      <div
        className={`xp-graph-canvas ${props.className ?? ""}`}
        data-testid={props["data-testid"]}
        onKeyDown={onKeyDown}
        role="group"
        aria-roledescription="canvas"
        aria-label={props["aria-label"]}
      >
        <Markers prefix={markerPrefix} />
        <ReactFlow<EntityRFNode, RelationRFEdge>
          nodes={flowNodes}
          edges={flowEdges}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          onConnect={handleConnect}
          onNodeDoubleClick={(_, node) => onActivateNode?.(node.id)}
          onMoveEnd={(_, next) => {
            if (sameViewport(next, lastViewport.current)) return;
            lastViewport.current = next;
            onViewportChange({ x: next.x, y: next.y, zoom: next.zoom });
          }}
          defaultViewport={viewport ?? undefined}
          connectionMode={ConnectionMode.Loose}
          nodesConnectable={Boolean(onConnect)}
          nodesDraggable
          nodesFocusable
          edgesFocusable
          elementsSelectable
          edgesReconnectable={false}
          deleteKeyCode={null}
          selectionKeyCode="Shift"
          multiSelectionKeyCode={["Control", "Meta"]}
          panActivationKeyCode="Space"
          panOnDrag={[0, 1]}
          panOnScroll={false}
          zoomOnScroll
          zoomOnPinch
          zoomOnDoubleClick={false}
          minZoom={0.1}
          maxZoom={2.5}
          nodeDragThreshold={2}
          proOptions={{ hideAttribution: true }}
          ariaLabelConfig={{
            "node.a11yDescription.default":
              "Press Enter or Space to select. Arrow keys move the selected item; moving changes the layout only.",
            "node.a11yDescription.keyboardDisabled": "Press Enter or Space to select.",
            "edge.a11yDescription.default": "Press Enter or Space to select this relationship.",
            "handle.ariaLabel": "Connection handle: drag to another item to propose a relationship",
          }}
        >
          <CanvasControls selection={selection.nodes} extra={props.controls} />
          {nodes.length === 0 && props.empty && (
            <Panel position="top-center" className="xp-graph-canvas__empty">
              {props.empty}
            </Panel>
          )}
        </ReactFlow>
      </div>
    </CanvasContext.Provider>
  );
}

/** Typed canvas over plain props; see the module comment for the interaction standard. */
export function GraphCanvas(props: GraphCanvasProps) {
  return (
    <ReactFlowProvider>
      <CanvasInner {...props} />
    </ReactFlowProvider>
  );
}
