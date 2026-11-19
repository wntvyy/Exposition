// Layout pieces shared by the settings sections (structure only; controls,
// headers, sections, and text roles come from src/ui/components).
import { useId, type ReactNode } from "react";

import { PageHeader, Section } from "../../ui/components";

/**
 * One settings page: the shared page header (title and page actions), then a
 * scrolling body at the reading width with the short description and groups.
 */
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
      <PageHeader title={title} titleId={id} actions={actions} />
      <div className="exp-settings__body">
        <div className="exp-settings__content">
          {description && <p className="exp-settings__description">{description}</p>}
          {children}
        </div>
      </div>
    </section>
  );
}

/**
 * A titled group of related settings within a page: a content Section (the
 * sentence-case section title, a muted count or fact, trailing actions) with
 * an optional short description before its rows.
 */
export function SettingsGroup({
  title,
  count,
  description,
  actions,
  children,
  testId,
}: {
  title: string;
  /** Muted fact after the title, e.g. "(2 · 3.0 MiB)". */
  count?: string;
  description?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  testId?: string;
}) {
  return (
    <Section title={title} count={count} actions={actions} data-testid={testId}>
      {description && <p className="exp-settings__group-description ui-meta">{description}</p>}
      {children}
    </Section>
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
          <div className="exp-settings__row-description ui-meta" id={descriptionId}>
            {description}
          </div>
        )}
      </div>
      <div className="exp-settings__row-control">{typeof children === "function" ? children(ids) : children}</div>
    </div>
  );
}
