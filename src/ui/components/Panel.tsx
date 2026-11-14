import { ChevronRight } from "lucide-react";
import { useId, useState, type ReactNode } from "react";
import { Toolbar as AriaToolbar, type ToolbarProps as AriaToolbarProps } from "react-aria-components";

import { cx } from "./cx";
import { StateSizeProvider } from "./stateSize";

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
  /** Draw the bottom divider (default true). */
  divider?: boolean;
}

/**
 * Pane chrome title row: the uppercase chrome label (pane titles are the only
 * place besides panel tab strips that uses uppercase) with trailing actions.
 * One height (`--pane-header-height`) and inset (`--pane-inset`) everywhere:
 * sidebar views, inspectors, drawers.
 */
export function PanelHeader({ title, id, actions, className, divider = true }: PanelHeaderProps) {
  return (
    <header className={cx("ui-panel-header", className)} data-divider={divider || undefined}>
      <h2 className="ui-panel-header__title ui-chrome-label" id={id}>
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
  title: ReactNode;
  children: ReactNode;
  /** Count or short fact shown after the title in muted text ("Relationships 3", "2 · 3.0 MiB"). */
  count?: number | string;
  /** Id of the heading (for aria-labelledby). */
  headingId?: string;
  "data-testid"?: string;
  /** Collapsible sections remember nothing; persist expansion in the owning view if needed. */
  collapsible?: boolean;
  defaultExpanded?: boolean;
  actions?: ReactNode;
  className?: string;
  /**
   * `content` (default): a section inside content (inspectors, dialogs, task
   * views): sentence-case 12px semibold title, aligned with its content.
   * `chrome`: a collapsible part of a pane's frame (e.g. a sidebar subsection):
   * uppercase chrome label.
   */
  variant?: "content" | "chrome";
}

/** Titled group within a panel, optionally collapsible (disclosure pattern). */
export function Section({
  title,
  children,
  count,
  collapsible = false,
  defaultExpanded = true,
  actions,
  className,
  variant = "content",
  headingId,
  ...rest
}: SectionProps) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  const bodyId = useId();
  const heading = (
    <h3 className={cx("ui-section__title", variant === "chrome" ? "ui-chrome-label" : "ui-section-title")} id={headingId}>
      {title}
      {count !== undefined && (
        <>
          {" "}
          <span className="ui-section__count">{count}</span>
        </>
      )}
    </h3>
  );
  return (
    <section
      className={cx("ui-section", className)}
      data-variant={variant}
      data-expanded={expanded || undefined}
      data-testid={rest["data-testid"]}
    >
      <div className="ui-section__header">
        {collapsible ? (
          <button
            type="button"
            className="ui-section__toggle"
            aria-expanded={expanded}
            aria-controls={bodyId}
            onClick={() => setExpanded((e) => !e)}
          >
            {/* Chrome sections hang the chevron before the label; content sections keep the
                title aligned with their content and put the chevron after it. */}
            {variant === "chrome" && <ChevronRight className="ui-section__chevron" size={14} aria-hidden />}
            {heading}
            {variant === "content" && <ChevronRight className="ui-section__chevron" size={14} aria-hidden />}
          </button>
        ) : (
          heading
        )}
        {actions && <div className="ui-section__actions">{actions}</div>}
      </div>
      <div className="ui-section__body" id={bodyId} hidden={!expanded}>
        {children}
      </div>
    </section>
  );
}

export interface PageHeaderProps {
  /** Page or document title (14px semibold). */
  title: ReactNode;
  /** Secondary text after the title (path, revision). Muted, truncates first. */
  subtitle?: ReactNode;
  /** View switcher (a SegmentedControl) right after the title. */
  views?: ReactNode;
  /** Actions (Buttons/IconButtons; one primary at most). */
  actions?: ReactNode;
  /** Trailing status and facts (StatusPill for the headline state, counts). */
  status?: ReactNode;
  /** Heading level of the title (default 2). */
  level?: 1 | 2;
  /** Id of the title heading (for aria-labelledby on the page). */
  titleId?: string;
  /** `data-region` for F6 cycling (e.g. "graph-header"). */
  region?: string;
  /** Test id passthrough. */
  "data-testid"?: string;
  className?: string;
  /** A second row under the header (filters, search), same inset. */
  toolbar?: ReactNode;
  /** `data-region` of the toolbar row. */
  toolbarRegion?: string;
}

/**
 * The one header for top-level pages (Research, Agents, Graph) and
 * document-like editors (workflow editor, Settings pages): title, optional
 * view switch, actions, and the headline status at the end. Same height,
 * inset, and type as every other page; an optional toolbar row below.
 */
export function PageHeader({
  title,
  subtitle,
  views,
  actions,
  status,
  level = 2,
  titleId,
  region,
  className,
  toolbar,
  toolbarRegion,
  ...rest
}: PageHeaderProps) {
  const Heading = level === 1 ? "h1" : "h2";
  return (
    <>
      <header className={cx("ui-page-header", className)} data-region={region} data-testid={rest["data-testid"]}>
        <div className="ui-page-header__lead">
          <Heading className="ui-page-header__title" id={titleId}>
            {title}
          </Heading>
          {subtitle && <span className="ui-page-header__subtitle">{subtitle}</span>}
        </div>
        {views && <div className="ui-page-header__views">{views}</div>}
        {actions && <div className="ui-page-header__actions">{actions}</div>}
        {status && <div className="ui-page-header__status">{status}</div>}
      </header>
      {toolbar && (
        <div className="ui-page-toolbar" data-region={toolbarRegion}>
          {toolbar}
        </div>
      )}
    </>
  );
}

export interface InspectorHeaderProps {
  /** Type indicator (icon + text) in its category color, e.g. Theorem. */
  type?: ReactNode;
  /** Item title (14px semibold). */
  title: ReactNode;
  /** Facts and headline state under the title. */
  meta?: ReactNode;
  /** Item actions (secondary/ghost Buttons; More menu). */
  actions?: ReactNode;
  className?: string;
}

/** The header of an inspected item: type, title, facts, actions. */
export function InspectorHeader({ type, title, meta, actions, className }: InspectorHeaderProps) {
  return (
    <div className={cx("ui-inspector-header", className)}>
      {type && <div className="ui-inspector-header__type">{type}</div>}
      <h3 className="ui-inspector-header__title">{title}</h3>
      {meta && <div className="ui-inspector-header__meta">{meta}</div>}
      {actions && <div className="ui-inspector-header__actions">{actions}</div>}
    </div>
  );
}

/**
 * Inspector body: a scrollable column of Sections with the shared inset.
 * Region states inside render compact.
 */
export function InspectorBody({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cx("ui-inspector-body", className)}>
      <StateSizeProvider value="compact">{children}</StateSizeProvider>
    </div>
  );
}

export interface KeyValueItem {
  key: string;
  label: ReactNode;
  value: ReactNode;
  /** Monospace value (paths, hashes, ids). */
  code?: boolean;
}

/** Label/value facts (Path, Kind, Digest…): one grid layout for every inspector and dialog. */
export function KeyValueList({ items, className }: { items: KeyValueItem[]; className?: string }) {
  return (
    <dl className={cx("ui-kv", className)}>
      {items.map((item) => (
        <div className="ui-kv__row" key={item.key}>
          <dt className="ui-kv__label">{item.label}</dt>
          <dd className="ui-kv__value" data-code={item.code || undefined}>
            {item.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}
