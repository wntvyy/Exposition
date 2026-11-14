// Wires a graph section (Project or Mathematics; canvas or tables) to the
// operation commands: its main region is a focus-scoped surface, items get a
// context menu (right click, Shift+F10 / Menu key), and the selection's
// actions come from `graphActions`.
import { useRef, type MouseEvent, type RefObject } from "react";

import type { Entity } from "../../bindings/Entity";
import type { Relation } from "../../bindings/Relation";
import { useContextMenu } from "../../ui/components";
import { OpContextMenu } from "../operations/OpMenu";
import { acceptsItems, useOpSurface } from "../operations/surfaces";
import type { CanvasSelection } from "./canvas/GraphCanvas";
import { graphActions } from "./graphOps";
import type { VocabularyIndex } from "./model";

export interface GraphOpsOptions {
  id: string;
  selection: CanvasSelection;
  entities: Map<string, Entity>;
  relations: Map<string, Relation>;
  vocabulary: VocabularyIndex | null;
  canWrite: boolean;
  onSelect: (selection: CanvasSelection) => void;
  onHide?: (ids: string[]) => void;
}

export interface GraphOps {
  ref: RefObject<HTMLDivElement | null>;
  /** For the canvas: an item (or the background, `null`) was right-clicked. */
  onItemContextMenu: (event: MouseEvent, item: { kind: "node" | "edge"; id: string } | null) => void;
  /** For tables and other DOM: finds `data-entity-id` / `data-relation-id`. */
  onContextMenu: (event: MouseEvent) => void;
  menu: React.ReactNode;
}

export function useGraphOps(options: GraphOpsOptions): GraphOps {
  const ref = useRef<HTMLDivElement>(null);
  const menu = useContextMenu();
  const latest = useRef(options);
  latest.current = options;

  const compute = () => {
    const o = latest.current;
    const entities = o.selection.nodes.flatMap((id) => {
      const e = o.entities.get(id);
      return e ? [e] : [];
    });
    const relation = o.selection.edge ? o.relations.get(o.selection.edge) : undefined;
    return graphActions(
      { entities, relations: relation ? [relation] : [] },
      {
        vocabulary: o.vocabulary,
        canWrite: o.canWrite,
        onHide: o.onHide,
        afterCreate: (ids) => o.onSelect({ nodes: ids.filter((id) => o.entities.has(id)), edge: null }),
      },
    );
  };
  useOpSurface(options.id, ref, () => compute().actions, { accepts: acceptsItems });

  const onItemContextMenu = (event: MouseEvent, item: { kind: "node" | "edge"; id: string } | null) => {
    const o = latest.current;
    if (item?.kind === "node" && !o.selection.nodes.includes(item.id)) o.onSelect({ nodes: [item.id], edge: null });
    if (item?.kind === "edge" && o.selection.edge !== item.id) o.onSelect({ nodes: [], edge: item.id });
    menu.openFromEvent(event);
  };

  const onContextMenu = (event: MouseEvent) => {
    const target = event.target instanceof Element ? event.target : null;
    const entity = target?.closest("[data-entity-id]")?.getAttribute("data-entity-id");
    const relation = target?.closest("[data-relation-id]")?.getAttribute("data-relation-id");
    if (entity) onItemContextMenu(event, { kind: "node", id: entity });
    else if (relation) onItemContextMenu(event, { kind: "edge", id: relation });
  };

  const current = menu.position ? compute() : null;
  return {
    ref,
    onItemContextMenu,
    onContextMenu,
    menu: current ? <OpContextMenu controller={menu} label="Graph actions" actions={current.actions} extras={current.extras} testId="graph-context-menu" /> : null,
  };
}
