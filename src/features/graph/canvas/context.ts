import { createContext, useContext } from "react";

/** Canvas-wide settings and callbacks shared by its custom nodes and edges. */
export interface CanvasContextValue {
  /** Prefix for SVG marker ids (unique per canvas instance). */
  markerPrefix: string;
  /** Nodes offer a connection handle (typed relationship authoring). */
  connectable: boolean;
  /** Edge labels: always, or only for the selection and its neighborhood. */
  labels: "all" | "selection";
  /** Ids of the selected nodes (edges touching them show their labels). */
  selectedNodes: ReadonlySet<string>;
  onToggleGroup?: (id: string) => void;
  onSelectEdge: (id: string) => void;
}

export const CanvasContext = createContext<CanvasContextValue>({
  markerPrefix: "xp-graph",
  connectable: false,
  labels: "selection",
  selectedNodes: new Set(),
  onSelectEdge: () => {},
});

export const useCanvasContext = () => useContext(CanvasContext);
