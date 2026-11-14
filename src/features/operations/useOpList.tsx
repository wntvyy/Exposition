// Operation support for card lists (Workflows, runs, agent tasks): each card
// is a focusable item; click selects it (Ctrl/Cmd+click adds or removes),
// the operation shortcuts act on the selection while a card has focus, and
// the card's "More actions" button and right click open the same menu.
import { MoreHorizontal } from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";

import { IconButton, Menu, MenuTrigger, useContextMenu } from "../../ui/components";
import { OpContextMenu, opMenuItems, runMenuKey, type ExtraItem } from "./OpMenu";
import { useOpSurface, type SurfaceActions } from "./surfaces";

export interface OpListOptions<T> {
  id: string;
  label: string;
  items: T[];
  keyOf: (item: T) => string;
  /** Actions (and extra menu items) for the selected items. */
  actionsFor: (selected: T[]) => { actions: SurfaceActions; extras?: ExtraItem[] };
}

export function useOpList<T>(options: OpListOptions<T>) {
  const ref = useRef<HTMLDivElement>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const menu = useContextMenu();
  const latest = useRef(options);
  latest.current = options;

  // Forget selected items that are gone.
  const keys = new Set(options.items.map(options.keyOf));
  const stale = selected.some((k) => !keys.has(k));
  useEffect(() => {
    if (stale) setSelected((current) => current.filter((k) => new Set(latest.current.items.map(latest.current.keyOf)).has(k)));
  }, [stale]);

  const pointerDown = useRef(false);
  const selectedRef = useRef(selected);
  selectedRef.current = selected;
  /**
   * What the keys act on: the selection, unless focus is on a card outside
   * it — then that card alone (the focused item is what the person sees).
   */
  const effectiveKeys = (): string[] => {
    const focused = typeof document === "undefined" ? null : document.activeElement?.closest("[data-op-item]")?.getAttribute("data-op-item");
    const current = selectedRef.current;
    return focused && ref.current?.contains(document.activeElement) && !current.includes(focused) ? [focused] : current;
  };
  const selection = (keysToUse: string[]) => {
    const o = latest.current;
    return o.items.filter((item) => keysToUse.includes(o.keyOf(item)));
  };
  const compute = (keysToUse: string[] = effectiveKeys()) => latest.current.actionsFor(selection(keysToUse));

  useOpSurface(options.id, ref, () => compute().actions, { accepts: (element) => element.closest("[data-op-item]") !== null });

  const select = (key: string, extend: boolean) =>
    setSelected((current) => (extend ? (current.includes(key) ? current.filter((k) => k !== key) : [...current, key]) : [key]));

  const itemProps = (key: string) => ({
    "data-op-item": key,
    "data-op-selected": selected.includes(key) || undefined,
    tabIndex: 0,
    className: "xp-ops-item",
    onMouseDown: () => {
      pointerDown.current = true;
    },
    onClick: (event: MouseEvent) => {
      pointerDown.current = false;
      // Buttons and links inside the card keep their own action (and select the card alone).
      if (event.target instanceof Element && event.target.closest("button, a, input, textarea, [role='button']")) {
        if (!selectedRef.current.includes(key)) setSelected([key]);
        return;
      }
      select(key, event.ctrlKey || event.metaKey);
    },
    onFocus: () => {
      // Keyboard focus on a card (or a button in it) selects it, unless it is already part of the selection.
      if (!pointerDown.current && !selectedRef.current.includes(key)) setSelected([key]);
    },
    onKeyDown: (event: KeyboardEvent) => {
      if (event.target !== event.currentTarget) return;
      if (event.key === " " && !event.repeat) {
        event.preventDefault();
        select(key, event.ctrlKey || event.metaKey);
      }
    },
    onContextMenu: (event: MouseEvent) => {
      if (!selected.includes(key)) setSelected([key]);
      menu.openFromEvent(event);
    },
  });

  /** A "More actions" menu button for one card (acts on that card). */
  const moreButton = (key: string, label: string) => {
    const current = compute([key]);
    return (
      <MenuTrigger>
        <IconButton icon={MoreHorizontal} size="sm" aria-label={`More actions for ${label}`} data-testid={`op-more-${key}`} />
        <Menu aria-label={`Actions for ${label}`} onAction={(k) => runMenuKey(k, current.actions, current.extras)}>
          {opMenuItems(current.actions, current.extras)}
        </Menu>
      </MenuTrigger>
    );
  };

  const current = menu.position ? compute() : null;
  const contextMenu = current ? (
    <OpContextMenu controller={menu} label={options.label} actions={current.actions} extras={current.extras} testId={`${options.id}-context-menu`} />
  ) : null;

  return { ref, selected, setSelected, itemProps, moreButton, contextMenu };
}
