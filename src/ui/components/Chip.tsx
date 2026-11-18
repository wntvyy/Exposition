import { X } from "lucide-react";
import type { HTMLAttributes, ReactNode } from "react";

import { IconButton, type IconComponent } from "./Button";
import { cx } from "./cx";

export type ChipTone = "neutral" | "accent" | "warning";

export interface ChipProps extends Omit<HTMLAttributes<HTMLSpanElement>, "children" | "className" | "title"> {
  /** The item's name; truncates, with the full text as its title. */
  label: string;
  /** Leading icon (lucide-react component or FileIcon-like component). */
  icon?: IconComponent;
  /** Leading thumbnail node (an image preview) instead of an icon; 20px. */
  thumbnail?: ReactNode;
  /** Muted facts after the label (size, page count). */
  meta?: ReactNode;
  /** Renders a remove button named `removeLabel` (default "Remove <label>"). */
  onRemove?: () => void;
  removeLabel?: string;
  /** `neutral` (attachments), `accent` (context the next message is about), `warning` (an item that needs attention). */
  tone?: ChipTone;
  isDisabled?: boolean;
  className?: string;
}

/**
 * Compact removable item: an attachment, or context such as "About: <question>".
 * Interactive content, not a fact label (that is `Badge`). Height 24, chip radius.
 */
export function Chip({
  label,
  icon: Icon,
  thumbnail,
  meta,
  onRemove,
  removeLabel,
  tone = "neutral",
  isDisabled,
  className,
  ...rest
}: ChipProps) {
  return (
    <span
      {...rest}
      className={cx("ui-chip", className)}
      data-tone={tone}
      data-disabled={isDisabled || undefined}
      data-removable={onRemove ? true : undefined}
    >
      {thumbnail ? (
        <span className="ui-chip__thumbnail" aria-hidden>
          {thumbnail}
        </span>
      ) : (
        Icon && <Icon size={14} aria-hidden className="ui-chip__icon" />
      )}
      <span className="ui-chip__label" title={label}>
        {label}
      </span>
      {meta !== undefined && meta !== null && meta !== false && <span className="ui-chip__meta">{meta}</span>}
      {onRemove && (
        <IconButton
          icon={X}
          size="sm"
          className="ui-chip__remove"
          aria-label={removeLabel ?? `Remove ${label}`}
          isDisabled={isDisabled}
          onPress={onRemove}
        />
      )}
    </span>
  );
}
