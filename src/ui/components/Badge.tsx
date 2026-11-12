import type { ReactNode } from "react";

import type { IconComponent } from "./Button";
import { cx } from "./cx";

/**
 * Badge tones. `neutral` for facts (a count, a kind), feedback tones for
 * states, `stale` (dashed, violet) for out-of-date, `accent` for a proposed or
 * suggested item that is not yet accepted.
 */
export type BadgeTone = "neutral" | "info" | "success" | "warning" | "danger" | "stale" | "accent";

export interface BadgeProps {
  tone?: BadgeTone;
  icon?: IconComponent;
  /** Dashed outline: a provisional or uncertain fact (suggested, unknown, unresolved). */
  dashed?: boolean;
  children: ReactNode;
  className?: string;
  /** Full description when the visible text is abbreviated. */
  title?: string;
}

/**
 * Small inline label for a fact or state attached to an item (a node, a row):
 * "Suggested", "Unresolved binding", "3 relations". Always text, optionally
 * with an icon; never color alone. Lifecycle status of builds/runs/checks uses
 * `StatusPill` instead.
 */
export function Badge({ tone = "neutral", icon: Icon, dashed, children, className, title }: BadgeProps) {
  return (
    <span className={cx("ui-badge", className)} data-tone={tone} data-dashed={dashed || undefined} title={title}>
      {Icon && <Icon size={12} aria-hidden />}
      <span className="ui-badge__label">{children}</span>
    </span>
  );
}
