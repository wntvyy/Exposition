import { useCallback, useRef, useState, type MouseEvent, type ReactNode } from "react";
import { Menu as AriaMenu, Popover as AriaPopover, type Key } from "react-aria-components";

export interface ContextMenuController {
  /** Where the menu is open (viewport coordinates), or null. */
  position: { x: number; y: number } | null;
  /** Open at a pointer position, or under the focused element for keyboard (Shift+F10 / Menu key). */
  openFromEvent: (event: MouseEvent) => void;
  openAt: (x: number, y: number) => void;
  close: (restoreFocus?: boolean) => void;
}

/**
 * State for a context menu. Call `openFromEvent` from `onContextMenu` (after
 * updating the selection for the item under the pointer); the browser's own
 * menu is suppressed only while ours opens.
 */
export function useContextMenu(): ContextMenuController {
  const [position, setPosition] = useState<{ x: number; y: number } | null>(null);
  const restore = useRef<HTMLElement | null>(null);
  const openAt = useCallback((x: number, y: number) => {
    const active = document.activeElement;
    restore.current = active instanceof HTMLElement ? active : null;
    setPosition({ x, y });
  }, []);
  const openFromEvent = useCallback(
    (event: MouseEvent) => {
      event.preventDefault();
      event.stopPropagation();
      // A keyboard-invoked context menu (Shift+F10, the Menu key) has no pointer position.
      const keyboard = event.button !== 2 && event.clientX === 0 && event.clientY === 0;
      if (keyboard) {
        const target = document.activeElement instanceof HTMLElement ? document.activeElement : (event.target as HTMLElement);
        const rect = target.getBoundingClientRect();
        openAt(rect.left + 8, rect.bottom);
      } else {
        openAt(event.clientX, event.clientY);
      }
    },
    [openAt],
  );
  const close = useCallback((restoreFocus = true) => {
    setPosition(null);
    const target = restore.current;
    restore.current = null;
    if (restoreFocus && target?.isConnected) target.focus({ preventScroll: true });
  }, []);
  return { position, openFromEvent, openAt, close };
}

export interface ContextMenuProps {
  controller: ContextMenuController;
  "aria-label": string;
  /** Runs after the menu closed and focus returned to where it was. */
  onAction: (key: Key) => void;
  /** `MenuItem`s, `MenuSection`s, `MenuSeparator`s. */
  children: ReactNode;
  disabledKeys?: Iterable<Key>;
  "data-testid"?: string;
}

/** A context menu at the pointer (or under the focused item for keyboard), built on the shared menu styling. */
export function ContextMenu({ controller, onAction, children, disabledKeys, ...props }: ContextMenuProps) {
  const anchor = useRef<HTMLDivElement>(null);
  const { position, close } = controller;
  return (
    <>
      <div
        ref={anchor}
        aria-hidden="true"
        style={{ position: "fixed", left: position?.x ?? 0, top: position?.y ?? 0, width: 1, height: 1, pointerEvents: "none" }}
      />
      <AriaPopover
        triggerRef={anchor}
        isOpen={position !== null}
        onOpenChange={(open) => {
          if (!open) close();
        }}
        placement="bottom start"
        offset={2}
        className="ui-popover"
      >
        <AriaMenu
          aria-label={props["aria-label"]}
          data-testid={props["data-testid"]}
          className="ui-menu"
          autoFocus="first"
          disabledKeys={disabledKeys}
          onAction={(key) => {
            close();
            onAction(key);
          }}
        >
          {children}
        </AriaMenu>
      </AriaPopover>
    </>
  );
}
