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
  /** Status badges, separate from the type (`Badge` with an icon). */
  badges?: ReactNode;
  /** Trailing control in the header (e.g. a collapse toggle). */
  action?: ReactNode;
  selected?: boolean;
  /** De-emphasized (e.g. not matching the current search). */
  dimmed?: boolean;
  /** Archived/inactive item: dashed outline. */
  inactive?: boolean;
  className?: string;
  /** Extra rows below the body that reach the card edges (e.g. workflow port rows with handles). */
  children?: ReactNode;
}

/**
 * The rectangle used for items on canvases (project graph, Mathematics
 * dependencies, workflow steps): a neutral card whose type header band
 * carries the category color (icon + type label as text), a concise title,
 * and separate status badges. Type color never colors the body, the outline,
 * or the selection, so statuses and selection stay distinct from types.
 * Purely presentational; canvases own positioning, selection, and focus.
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
  children,
}: NodeCardProps) {
  return (
    <div
      className={cx("ui-node-card", className)}
      data-category={category}
      data-selected={selected || undefined}
      data-dimmed={dimmed || undefined}
      data-inactive={inactive || undefined}
      data-has-extra={children ? true : undefined}
    >
      <div className="ui-node-card__header">
        <span className="ui-node-card__type">
          {Icon && <Icon size={12} aria-hidden />}
          <span>{typeLabel}</span>
        </span>
        {action}
      </div>
      <div className="ui-node-card__body">
        <p className="ui-node-card__title" title={title}>
          {title}
        </p>
        {badges && <div className="ui-node-card__badges">{badges}</div>}
      </div>
      {children && <div className="ui-node-card__extra">{children}</div>}
    </div>
  );
}
