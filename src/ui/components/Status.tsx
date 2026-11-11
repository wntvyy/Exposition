import {
  Ban,
  CircleCheck,
  CircleDashed,
  CircleSlash,
  CircleX,
  History,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { ProgressBar as AriaProgressBar, Label } from "react-aria-components";

import { cx } from "./cx";

export interface SpinnerProps {
  size?: "sm" | "md" | "lg";
  /** Accessible name; pass `aria-hidden` instead when a visible label already says "Loading…". */
  "aria-label"?: string;
  "aria-hidden"?: boolean;
  className?: string;
}

const spinnerPx = { sm: 14, md: 16, lg: 24 } as const;

/** Indeterminate activity indicator (role=progressbar). */
export function Spinner({ size = "md", className, ...props }: SpinnerProps) {
  const px = spinnerPx[size];
  const svg = (
    <svg className="ui-spinner__svg" width={px} height={px} viewBox="0 0 16 16" aria-hidden>
      <circle cx="8" cy="8" r="6.5" className="ui-spinner__track" />
      <path d="M8 1.5 A6.5 6.5 0 0 1 14.5 8" className="ui-spinner__head" />
    </svg>
  );
  if (props["aria-hidden"]) {
    return (
      <span className={cx("ui-spinner", className)} aria-hidden>
        {svg}
      </span>
    );
  }
  return (
    <AriaProgressBar isIndeterminate aria-label={props["aria-label"] ?? "Loading"} className={cx("ui-spinner", className)}>
      {svg}
    </AriaProgressBar>
  );
}

export interface ProgressBarProps {
  label: string;
  /** 0–100; omit for indeterminate progress. */
  value?: number;
  /** Visible value text, e.g. "3 of 12 pages". Defaults to a percentage. */
  valueLabel?: string;
  tone?: "accent" | "danger";
  className?: string;
}

/** Determinate or indeterminate progress with a visible label. */
export function ProgressBar({ label, value, valueLabel, tone = "accent", className }: ProgressBarProps) {
  return (
    <AriaProgressBar
      className={cx("ui-progress", className)}
      value={value}
      isIndeterminate={value === undefined}
      valueLabel={valueLabel}
      data-tone={tone}
    >
      {({ percentage, valueText, isIndeterminate }) => (
        <>
          <div className="ui-progress__header">
            <Label className="ui-progress__label">{label}</Label>
            {!isIndeterminate && <span className="ui-progress__value">{valueText}</span>}
          </div>
          <div className="ui-progress__track">
            <div
              className="ui-progress__fill"
              data-indeterminate={isIndeterminate || undefined}
              style={isIndeterminate ? undefined : { width: `${percentage ?? 0}%` }}
            />
          </div>
        </>
      )}
    </AriaProgressBar>
  );
}

/** Lifecycle status for builds, runs, checks, agent tasks, and tools. */
export type Status = "idle" | "running" | "succeeded" | "failed" | "canceled" | "stale" | "unavailable" | "warning";

export const statusLabels: Record<Status, string> = {
  idle: "Idle",
  running: "Running",
  succeeded: "Succeeded",
  failed: "Failed",
  canceled: "Canceled",
  stale: "Stale",
  unavailable: "Unavailable",
  warning: "Warning",
};

const statusIcons: Record<Exclude<Status, "running">, LucideIcon> = {
  idle: CircleDashed,
  succeeded: CircleCheck,
  failed: CircleX,
  canceled: CircleSlash,
  stale: History,
  unavailable: Ban,
  warning: TriangleAlert,
};

export interface StatusPillProps {
  status: Status;
  /** Visible text; defaults to the status name ("Running", "Failed", …). Be specific: "Build failed". */
  label?: string;
  /** Extra detail after the label, e.g. "12s" or "rev 14". */
  detail?: string;
  size?: "sm" | "md";
  className?: string;
}

/**
 * Compact status indicator: icon + text, never color alone. Running shows a
 * spinner. Wrap changing pills in a live region only when the change matters
 * to the user (notifications already announce completions).
 */
export function StatusPill({ status, label, detail, size = "md", className }: StatusPillProps) {
  const Icon = status === "running" ? null : statusIcons[status];
  return (
    <span className={cx("ui-status-pill", className)} data-status={status} data-size={size}>
      {Icon ? <Icon size={size === "sm" ? 12 : 14} aria-hidden /> : <Spinner size="sm" aria-hidden />}
      <span className="ui-status-pill__label">{label ?? statusLabels[status]}</span>
      {detail && <span className="ui-status-pill__detail">{detail}</span>}
    </span>
  );
}
