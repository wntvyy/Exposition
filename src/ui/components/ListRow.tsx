import { forwardRef, type HTMLAttributes, type ReactNode } from "react";

import { cx } from "./cx";

export interface ListRowProps extends Omit<HTMLAttributes<HTMLDivElement>, "title"> {
  /** Leading icon (14–16px lucide icon element). */
  icon?: ReactNode;
  /** Primary line (13px). */
  title: ReactNode;
  /** Secondary line: StatusText, muted facts, time. */
  meta?: ReactNode;
  /** Trailing content: counts, a More actions button. */
  trailing?: ReactNode;
  /** Selected (accent tint while its list has focus, neutral otherwise). */
  selected?: boolean;
  /** Monospace title (code snippets, paths). */
  codeTitle?: boolean;
}

/**
 * One row of a list that is not a tree or a table: tasks, runs, workflows,
 * evidence, storage items. Flat (no card box): a divider between rows,
 * hover and selected fills, the inset focus ring. Interactivity (click,
 * keyboard, P1 operation attributes) stays with the owning list and is passed
 * through as ordinary attributes.
 */
export const ListRow = forwardRef<HTMLDivElement, ListRowProps>(function ListRow(
  { icon, title, meta, trailing, selected, codeTitle, className, children, ...rest },
  ref,
) {
  return (
    <div ref={ref} className={cx("ui-list-row", className)} data-selected={selected || undefined} {...rest}>
      {icon && <span className="ui-list-row__icon">{icon}</span>}
      <div className="ui-list-row__main">
        <div className="ui-list-row__title" data-code={codeTitle || undefined}>
          {title}
        </div>
        {meta && <div className="ui-list-row__meta">{meta}</div>}
        {children}
      </div>
      {trailing && <div className="ui-list-row__trailing">{trailing}</div>}
    </div>
  );
});
