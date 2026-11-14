import { createContext, useContext } from "react";

import { createInterestStore, type InterestStore } from "./interaction";

/** Canvas-wide settings and callbacks shared by its custom nodes and edges. */
export interface CanvasContextValue {
  /** Prefix for SVG marker ids (unique per canvas instance). */
  markerPrefix: string;
  /** Nodes offer a connection handle (typed relationship authoring). */
  connectable: boolean;
  /** Resting edge labels: shown (small graphs), or only for the selection and its neighborhood. */
  labels: "all" | "selection";
  /** Ids of the selected nodes (edges touching them show their labels). */
  selectedNodes: ReadonlySet<string>;
  /** Hovered and focused items (edges touching them show their labels). */
  interest: InterestStore;
  onToggleGroup?: (id: string) => void;
  onSelectEdge: (id: string) => void;
}

export const CanvasContext = createContext<CanvasContextValue>({
  markerPrefix: "xp-graph",
  connectable: false,
  labels: "selection",
  selectedNodes: new Set(),
  interest: createInterestStore(),
  onSelectEdge: () => {},
});

export const useCanvasContext = () => useContext(CanvasContext);
