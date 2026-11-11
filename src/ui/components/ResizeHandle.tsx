import type { KeyboardEvent } from "react";
import { Separator } from "react-resizable-panels";

import { cx } from "./cx";

export interface ResizeHandleProps {
  /** Accessible name, e.g. "Resize sidebar". */
  "aria-label": string;
  /** Direction of the handle bar: a vertical bar resizes horizontally. */
  orientation?: "vertical" | "horizontal";
  id?: string;
  className?: string;
  /**
   * F6 is the workbench "focus next region" key; the panel library would
   * otherwise cycle between handles. Supply the workbench handler here.
   */
  onRegionKey?: (backwards: boolean) => void;
}

/**
 * Splitter between two panes (react-resizable-panels `Separator`, WAI-ARIA
 * window-splitter). Focusable; Arrow keys resize in 5% steps, Home/End jump to
 * min/max, double-click resets. Must be a direct child of a `Group`.
 */
export function ResizeHandle({ orientation = "vertical", className, onRegionKey, ...props }: ResizeHandleProps) {
  const onKeyDownCapture = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "F6" && onRegionKey) {
      event.preventDefault();
      event.stopPropagation();
      onRegionKey(event.shiftKey);
    }
  };
  return (
    <Separator
      {...props}
      className={cx("ui-resize-handle", className)}
      data-orientation={orientation}
      onKeyDownCapture={onKeyDownCapture}
    />
  );
}
