// A saved view's canvas: projection nodes placed at their saved positions;
// nodes without one are placed beside the existing layout (never moving the
// user's positioned nodes) and that placement is saved. Moves, pans/zooms, and
// collapsed groups save to the view; Arrange is explicit and undoable.
import { useEffect, useMemo, useState, type ReactNode } from "react";

import { notify } from "../../app/notifications";
import { GraphCanvas, type CanvasSelection } from "./canvas/GraphCanvas";
import { layeredLayout, placeUnpositioned } from "./canvas/layout";
import { DEFAULT_NODE_SIZE, type CanvasNode, type Point } from "./canvas/types";
import type { Projection } from "./projection";
import type { ViewStore } from "./viewStore";

export interface ViewCanvasProps {
  "aria-label": string;
  "data-testid"?: string;
  store: ViewStore;
  projection: Projection;
  selection: CanvasSelection;
  onSelectionChange: (selection: CanvasSelection) => void;
  onConnect?: (source: string, target: string) => void;
  onActivateNode?: (id: string) => void;
  fitRequest?: { nonce: number; nodes?: string[] };
  empty?: ReactNode;
  labels?: "all" | "selection";
}

export function ViewCanvas(props: ViewCanvasProps) {
  const { store, projection } = props;
  const view = store.use((s) => s.view);
  const positions = view?.positions;

  // Positions for nodes the view has not placed yet (computed, then saved).
  const placed = useMemo(() => {
    if (!positions) return new Map<string, Point>();
    const positioned = projection.nodes
      .filter((n) => positions[n.id])
      .map((n) => ({ id: n.id, position: positions[n.id]!, size: sizeOf(positions[n.id]) }));
    const missing = projection.nodes.filter((n) => !positions[n.id]).map((n) => ({ id: n.id }));
    return placeUnpositioned(positioned, missing, projection.edges);
  }, [projection, positions]);

  useEffect(() => {
    if (placed.size) store.placeNodes(placed);
  }, [placed, store]);

  const nodes: CanvasNode[] = useMemo(
    () =>
      projection.nodes.map((n) => {
        const saved = positions?.[n.id];
        return { ...n, position: saved ? { x: saved.x, y: saved.y } : (placed.get(n.id) ?? { x: 0, y: 0 }) };
      }),
    [projection.nodes, positions, placed],
  );

  if (!view) return null;
  return (
    <GraphCanvas
      aria-label={props["aria-label"]}
      data-testid={props["data-testid"]}
      nodes={nodes}
      edges={projection.edges}
      viewport={view.viewport}
      selection={props.selection}
      labels={props.labels ?? (projection.edges.length <= 60 ? "all" : "selection")}
      onSelectionChange={props.onSelectionChange}
      onNodesMoved={(moves) => store.moveNodes(moves)}
      onViewportChange={(viewport) => store.setViewport(viewport)}
      onConnect={props.onConnect}
      onToggleGroup={(id) => store.toggleCollapsed(id)}
      onActivateNode={props.onActivateNode}
      fitRequest={props.fitRequest}
      empty={props.empty}
    />
  );
}

function sizeOf(layout: { width?: number; height?: number } | undefined) {
  return layout?.width && layout?.height ? { width: layout.width, height: layout.height } : undefined;
}

/**
 * Explicit "Arrange": a deterministic layered layout of the visible nodes,
 * saved to the view, with an Undo that restores the previous positions.
 */
export function arrange(store: ViewStore, projection: Projection, onArranged: () => void): void {
  const view = store.use.getState().view;
  if (!view || projection.nodes.length === 0) return;
  const previous = new Map<string, Point>();
  for (const n of projection.nodes) {
    const p = view.positions[n.id];
    if (p) previous.set(n.id, { x: p.x, y: p.y });
  }
  const next = layeredLayout(
    projection.nodes.map((n) => ({ id: n.id, size: sizeOf(view.positions[n.id]) ?? DEFAULT_NODE_SIZE })),
    projection.edges,
  );
  store.placeNodes(next, { replace: true });
  onArranged();
  notify({
    id: "graph-arranged",
    tone: "info",
    title: `Arranged ${projection.nodes.length} items`,
    body: "Positions changed for this view only; relationships are unchanged.",
    actions: [{ label: "Undo", run: () => store.placeNodes(previous, { replace: true }) }],
  });
}

/** Selection state helper for canvases and tables. */
export function useSelection() {
  return useState<CanvasSelection>({ nodes: [], edge: null });
}
