// Layout pieces shared by the settings sections (structure only; controls come
// from src/ui/components).
import { useId, type ReactNode } from "react";

import { cx } from "../../ui/components";

/** One settings page: title, short description, then groups. */
export function SettingsPage({
  title,
  description,
  actions,
  children,
  testId,
}: {
  title: string;
  description?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  testId?: string;
}) {
  const id = useId();
  return (
    <section className="exp-settings__page" aria-labelledby={id} data-testid={testId}>
      <header className="exp-settings__page-header">
        <div className="exp-settings__page-heading">
          <h2 id={id} className="exp-settings__page-title">
            {title}
          </h2>
          {description && <p className="exp-settings__page-description">{description}</p>}
        </div>
        {actions && <div className="exp-settings__page-actions">{actions}</div>}
      </header>
      {children}
    </section>
  );
}

/** A titled group of related settings within a page. */
export function SettingsGroup({
  title,
  description,
  actions,
  children,
  className,
  testId,
}: {
  title: string;
  description?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
  testId?: string;
}) {
  const id = useId();
  return (
    <section className={cx("exp-settings__group", className)} aria-labelledby={id} data-testid={testId}>
      <div className="exp-settings__group-header">
        <h3 id={id} className="exp-settings__group-title">
          {title}
        </h3>
        {actions && <div className="exp-settings__group-actions">{actions}</div>}
      </div>
      {description && <p className="exp-settings__group-description">{description}</p>}
      {children}
    </section>
  );
}

export interface RowIds {
  /** Id of the visible row label (use as the control's `aria-labelledby`). */
  labelId: string;
  /** Id of the row description, when there is one (use as `aria-describedby`). */
  descriptionId: string | undefined;
}

/**
 * Label and description on the left, the control on the right. Pass a function
 * as `children` to label the control with the row's visible text.
 */
export function SettingRow({
  label,
  description,
  children,
  testId,
}: {
  label: ReactNode;
  description?: ReactNode;
  children: ReactNode | ((ids: RowIds) => ReactNode);
  testId?: string;
}) {
  const labelId = useId();
  const descriptionId = useId();
  const ids: RowIds = { labelId, descriptionId: description ? descriptionId : undefined };
  return (
    <div className="exp-settings__row" data-testid={testId}>
      <div className="exp-settings__row-text">
        <div className="exp-settings__row-label" id={labelId}>
          {label}
        </div>
        {description && (
          <div className="exp-settings__row-description" id={descriptionId}>
            {description}
          </div>
        )}
      </div>
      <div className="exp-settings__row-control">{typeof children === "function" ? children(ids) : children}</div>
    </div>
  );
}
