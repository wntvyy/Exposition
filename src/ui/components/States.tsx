import { Ban, CircleCheck, CircleSlash, CircleX, History, Info, TriangleAlert, type LucideIcon } from "lucide-react";
import { useId, type ReactNode } from "react";

import type { IconComponent } from "./Button";
import { cx } from "./cx";
import { Spinner } from "./Status";

interface StateBaseProps {
  title: string;
  description?: ReactNode;
  /** Buttons for the next useful step. */
  actions?: ReactNode;
  /** Fill the containing region and center the content (default true). */
  fill?: boolean;
  className?: string;
}

function StateLayout({
  kind,
  icon: Icon,
  title,
  description,
  actions,
  fill = true,
  className,
  role,
  media,
  children,
}: StateBaseProps & {
  kind: string;
  icon?: IconComponent | null;
  role?: string;
  /** Replaces the icon (e.g. a spinner). */
  media?: ReactNode;
  /** Extra content after the description (e.g. error details). */
  children?: ReactNode;
}) {
  const titleId = useId();
  return (
    <div
      className={cx("ui-state", className)}
      data-kind={kind}
      data-fill={fill || undefined}
      role={role}
      aria-labelledby={titleId}
    >
      {media ??
        (Icon && (
          <span className="ui-state__icon">
            <Icon size={24} aria-hidden />
          </span>
        ))}
      <p className="ui-state__title" id={titleId}>
        {title}
      </p>
      {description && <div className="ui-state__description">{description}</div>}
      {children}
      {actions && <div className="ui-state__actions">{actions}</div>}
    </div>
  );
}

/** Nothing here yet: no folder, no output, no results. Say what would populate it and how. */
export function EmptyState({ icon, ...props }: StateBaseProps & { icon?: IconComponent }) {
  return <StateLayout kind="empty" icon={icon} {...props} />;
}

/** Work in progress that blocks this region. Prefer inline spinners for small, non-blocking updates. */
export function LoadingState({ title = "Loading…", ...props }: Partial<StateBaseProps>) {
  return (
    <StateLayout kind="loading" title={title} role="status" media={<Spinner size="lg" aria-hidden />} {...props} />
  );
}

/**
 * A failure that prevents showing this region. Include the cause and a
 * recovery action. Announced as an alert unless `live` is false.
 */
export function ErrorState({ details, live = true, ...props }: StateBaseProps & { details?: string; live?: boolean }) {
  return (
    <StateLayout kind="error" icon={CircleX} role={live ? "alert" : undefined} {...props}>
      {details && <pre className="ui-state__details">{details}</pre>}
    </StateLayout>
  );
}

/**
 * A capability that cannot work here: a missing tool, an unconfigured
 * toolchain, or a feature not built yet. Name what is missing and offer the
 * action that resolves it (install guidance, configure path) when one exists.
 */
export function UnavailableState(props: StateBaseProps & { icon?: IconComponent }) {
  return <StateLayout kind="unavailable" icon={props.icon ?? Ban} {...props} />;
}

export type BannerTone = "info" | "success" | "warning" | "danger" | "stale" | "canceled";

const bannerIcons: Record<BannerTone, LucideIcon> = {
  info: Info,
  success: CircleCheck,
  warning: TriangleAlert,
  danger: CircleX,
  stale: History,
  canceled: CircleSlash,
};

export interface BannerProps {
  tone: BannerTone;
  title: string;
  children?: ReactNode;
  actions?: ReactNode;
  className?: string;
  /** Announce politely when it appears (e.g. output became stale while the user was working). */
  live?: boolean;
}

/**
 * Inline status message attached to a region: stale output, canceled run,
 * recoverable warnings. Does not block the content beneath it.
 */
export function Banner({ tone, title, children, actions, className, live }: BannerProps) {
  const Icon = bannerIcons[tone];
  return (
    <div className={cx("ui-banner", className)} data-tone={tone} role={live ? "status" : undefined}>
      <Icon className="ui-banner__icon" size={16} aria-hidden />
      <div className="ui-banner__body">
        <p className="ui-banner__title">{title}</p>
        {children && <div className="ui-banner__text">{children}</div>}
      </div>
      {actions && <div className="ui-banner__actions">{actions}</div>}
    </div>
  );
}
