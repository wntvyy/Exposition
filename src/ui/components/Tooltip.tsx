import type { ReactElement } from "react";
import { OverlayArrow, Tooltip as AriaTooltip, TooltipTrigger, type TooltipProps as AriaTooltipProps } from "react-aria-components";

import { keybindingLabel } from "./Kbd";

export interface TooltipProps {
  /** Tooltip text. For icon buttons this matches the accessible name. */
  label: string;
  /** Optional keybinding shown after the label. */
  keybinding?: string;
  placement?: AriaTooltipProps["placement"];
  /** A single focusable react-aria trigger (Button, ToggleButton, Link). */
  children: ReactElement;
  delay?: number;
}

/**
 * Hover/focus tooltip for supplementary text. Never put essential information
 * only in a tooltip; it is not reachable on touch and is transient.
 */
export function Tooltip({ label, keybinding, placement = "bottom", children, delay = 600 }: TooltipProps) {
  return (
    <TooltipTrigger delay={delay} closeDelay={100}>
      {children}
      <AriaTooltip className="ui-tooltip" placement={placement} offset={6}>
        <OverlayArrow className="ui-tooltip__arrow">
          <svg width={8} height={8} viewBox="0 0 8 8" aria-hidden>
            <path d="M0 0 L4 4 L8 0" />
          </svg>
        </OverlayArrow>
        {label}
        {keybinding && <span className="ui-tooltip__keys">{keybindingLabel(keybinding)}</span>}
      </AriaTooltip>
    </TooltipTrigger>
  );
}
