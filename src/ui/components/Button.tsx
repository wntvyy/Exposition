import type { ComponentType, ReactNode } from "react";
import {
  Button as AriaButton,
  ToggleButton as AriaToggleButton,
  ToggleButtonGroup as AriaToggleButtonGroup,
  type ButtonProps as AriaButtonProps,
  type Key,
  type ToggleButtonGroupProps as AriaToggleButtonGroupProps,
  type ToggleButtonProps as AriaToggleButtonProps,
} from "react-aria-components";

import { cx } from "./cx";
import { Spinner } from "./Status";
import { Tooltip } from "./Tooltip";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md" | "lg";

export type IconComponent = ComponentType<{ size?: number; "aria-hidden"?: boolean; className?: string }>;

export interface ButtonProps extends Omit<AriaButtonProps, "className" | "children"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Leading icon (lucide-react component). */
  icon?: IconComponent;
  className?: string;
  children?: ReactNode;
}

const iconSize = (size: ButtonSize) => (size === "lg" ? 16 : 14);

/**
 * Text button. `primary` for the single main action in a surface, `secondary`
 * for other actions, `ghost` for low-emphasis toolbar/inline actions, `danger`
 * for destructive confirmation. `isPending` shows a spinner and keeps focus.
 */
export function Button({ variant = "secondary", size = "md", icon: Icon, className, children, ...props }: ButtonProps) {
  return (
    <AriaButton {...props} className={cx("ui-button", className)} data-variant={variant} data-size={size}>
      {({ isPending }) => (
        <>
          {isPending ? <Spinner size="sm" aria-label="Working" /> : Icon && <Icon size={iconSize(size)} aria-hidden />}
          {children !== undefined && <span className="ui-button__label">{children}</span>}
        </>
      )}
    </AriaButton>
  );
}

export interface IconButtonProps extends Omit<AriaButtonProps, "className" | "children" | "aria-label"> {
  /** Required accessible name; also shown as the tooltip. */
  "aria-label": string;
  icon: IconComponent;
  variant?: "ghost" | "secondary";
  size?: ButtonSize;
  /** Keybinding shown in the tooltip, e.g. `Mod+B`. */
  keybinding?: string;
  /** Set false to suppress the tooltip (e.g. inside a labelled menu). */
  tooltip?: boolean;
  className?: string;
}

/** Icon-only button. Always has an accessible name and a tooltip with its shortcut. */
export function IconButton({
  icon: Icon,
  variant = "ghost",
  size = "md",
  keybinding,
  tooltip = true,
  className,
  ...props
}: IconButtonProps) {
  const button = (
    <AriaButton {...props} className={cx("ui-icon-button", className)} data-variant={variant} data-size={size}>
      <Icon size={size === "lg" ? 18 : 16} aria-hidden />
    </AriaButton>
  );
  if (!tooltip) return button;
  return (
    <Tooltip label={props["aria-label"]} keybinding={keybinding}>
      {button}
    </Tooltip>
  );
}

export interface ToggleButtonProps extends Omit<AriaToggleButtonProps, "className" | "children"> {
  icon?: IconComponent;
  size?: ButtonSize;
  className?: string;
  children?: ReactNode;
  /** Required when there is no visible text. */
  "aria-label"?: string;
}

/** Two-state button (pressed/unpressed), e.g. "Wrap lines". Selected state uses the accent tint. */
export function ToggleButton({ icon: Icon, size = "md", className, children, ...props }: ToggleButtonProps) {
  return (
    <AriaToggleButton {...props} className={cx("ui-toggle-button", className)} data-size={size}>
      {Icon && <Icon size={iconSize(size)} aria-hidden />}
      {children !== undefined && <span className="ui-button__label">{children}</span>}
    </AriaToggleButton>
  );
}

export interface SegmentedControlProps<K extends Key> extends Omit<
  AriaToggleButtonGroupProps,
  "className" | "children" | "selectedKeys" | "onSelectionChange" | "selectionMode"
> {
  "aria-label": string;
  value: K;
  onChange: (value: K) => void;
  options: { id: K; label: string; icon?: IconComponent }[];
  size?: ButtonSize;
  className?: string;
}

/** Single-choice toggle group, e.g. the Source / Page / Split mode switcher. */
export function SegmentedControl<K extends Key>({
  value,
  onChange,
  options,
  size = "sm",
  className,
  ...props
}: SegmentedControlProps<K>) {
  return (
    <AriaToggleButtonGroup
      {...props}
      className={cx("ui-segmented", className)}
      data-size={size}
      selectionMode="single"
      disallowEmptySelection
      selectedKeys={[value]}
      onSelectionChange={(keys) => {
        const next = [...keys][0];
        if (next !== undefined && next !== value) onChange(next as K);
      }}
    >
      {options.map(({ id, label, icon: Icon }) => (
        <AriaToggleButton key={String(id)} id={id} className="ui-segmented__item">
          {Icon && <Icon size={14} aria-hidden />}
          <span>{label}</span>
        </AriaToggleButton>
      ))}
    </AriaToggleButtonGroup>
  );
}
