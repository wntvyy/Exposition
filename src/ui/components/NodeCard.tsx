import type { ReactNode } from "react";

import type { GraphCategory } from "../tokens";
import type { IconComponent } from "./Button";
import { cx } from "./cx";

export interface NodeCardProps {
  /** Concise title (one or two lines, ellipsized). */
  title: string;
  /** Type indicator text, e.g. "Theorem" or "Folder". Always visible. */
  typeLabel: string;
  typeIcon?: IconComponent;
  /** Color category (tokens `--color-graph-<category>-*`). */
  category: GraphCategory;
  /** Status badges, separate from the type (`Badge` / `StatusPill size="sm"`). */
  badges?: ReactNode;
  /** Trailing control in the header (e.g. a collapse toggle). */
  action?: ReactNode;
  selected?: boolean;
  /** De-emphasized (e.g. not matching the current search). */
  dimmed?: boolean;
  /** Archived/inactive item: dashed outline. */
  inactive?: boolean;
  className?: string;
}

/**
 * The colored rectangle used for items on canvases (project graph, Mathematics
 * dependencies, later workflow steps): a type stripe and label in the
 * category's color, a concise title, and separate status badges. Purely
 * presentational; canvases own positioning, selection, and focus.
 */
export function NodeCard({
  title,
  typeLabel,
  typeIcon: Icon,
  category,
  badges,
  action,
  selected,
  dimmed,
  inactive,
  className,
}: NodeCardProps) {
  return (
    <div
      className={cx("ui-node-card", className)}
      data-category={category}
      data-selected={selected || undefined}
      data-dimmed={dimmed || undefined}
      data-inactive={inactive || undefined}
    >
      <div className="ui-node-card__header">
        <span className="ui-node-card__type">
          {Icon && <Icon size={12} aria-hidden />}
          <span>{typeLabel}</span>
        </span>
        {action}
      </div>
      <p className="ui-node-card__title" title={title}>
        {title}
      </p>
      {badges && <div className="ui-node-card__badges">{badges}</div>}
    </div>
  );
}
