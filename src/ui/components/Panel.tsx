import { ChevronRight } from "lucide-react";
import { useId, useState, type ReactNode } from "react";
import { Toolbar as AriaToolbar, type ToolbarProps as AriaToolbarProps } from "react-aria-components";

import { cx } from "./cx";

export interface ToolbarProps extends Omit<AriaToolbarProps, "className"> {
  /** Required: what the toolbar controls, e.g. "Build output actions". */
  "aria-label": string;
  className?: string;
}

/** Group of related controls; arrow keys move between them, Tab leaves the group. */
export function Toolbar({ className, ...props }: ToolbarProps) {
  return <AriaToolbar {...props} className={cx("ui-toolbar", className)} />;
}

export interface PanelProps {
  className?: string;
  children: ReactNode;
  /** Accessible name when no PanelHeader labels the panel. */
  "aria-label"?: string;
  "aria-labelledby"?: string;
}

/** A pane surface: header + scrollable body. Used for every view in sidebar/panel/auxiliary regions. */
export function Panel({ className, children, ...aria }: PanelProps) {
  return (
    <section className={cx("ui-panel", className)} {...aria}>
      {children}
    </section>
  );
}

export interface PanelHeaderProps {
  title: string;
  id?: string;
  /** Trailing actions (IconButtons inside a Toolbar). */
  actions?: ReactNode;
  className?: string;
}

/** Pane title row (uppercase label) with trailing actions. */
export function PanelHeader({ title, id, actions, className }: PanelHeaderProps) {
  return (
    <header className={cx("ui-panel-header", className)}>
      <h2 className="ui-panel-header__title" id={id}>
        {title}
      </h2>
      {actions && <div className="ui-panel-header__actions">{actions}</div>}
    </header>
  );
}

/** Scrollable body of a Panel. */
export function PanelBody({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cx("ui-panel-body", className)}>{children}</div>;
}

export interface SectionProps {
  title: string;
  children: ReactNode;
  /** Collapsible sections remember nothing; persist expansion in the owning view if needed. */
  collapsible?: boolean;
  defaultExpanded?: boolean;
  actions?: ReactNode;
  className?: string;
}

/** Titled group within a panel, optionally collapsible (disclosure pattern). */
export function Section({ title, children, collapsible = false, defaultExpanded = true, actions, className }: SectionProps) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  const bodyId = useId();
  return (
    <section className={cx("ui-section", className)} data-expanded={expanded || undefined}>
      <div className="ui-section__header">
        {collapsible ? (
          <button
            type="button"
            className="ui-section__toggle"
            aria-expanded={expanded}
            aria-controls={bodyId}
            onClick={() => setExpanded((e) => !e)}
          >
            <ChevronRight className="ui-section__chevron" size={14} aria-hidden />
            <h3 className="ui-section__title">{title}</h3>
          </button>
        ) : (
          <h3 className="ui-section__title">{title}</h3>
        )}
        {actions && <div className="ui-section__actions">{actions}</div>}
      </div>
      <div className="ui-section__body" id={bodyId} hidden={!expanded}>
        {children}
      </div>
    </section>
  );
}
