import { Check, ChevronRight } from "lucide-react";
import type { ReactNode } from "react";
import {
  Header,
  Keyboard,
  Menu as AriaMenu,
  MenuItem as AriaMenuItem,
  MenuSection as AriaMenuSection,
  MenuTrigger as AriaMenuTrigger,
  Popover as AriaPopover,
  Separator,
  SubmenuTrigger as AriaSubmenuTrigger,
  Text,
  type MenuItemProps as AriaMenuItemProps,
  type MenuProps as AriaMenuProps,
  type MenuTriggerProps,
  type PopoverProps as AriaPopoverProps,
} from "react-aria-components";

import type { IconComponent } from "./Button";
import { cx } from "./cx";
import { keybindingLabel } from "./Kbd";

/** Opens a Menu from a Button/IconButton child. */
export function MenuTrigger(props: MenuTriggerProps) {
  return <AriaMenuTrigger {...props} />;
}

export const SubmenuTrigger = AriaSubmenuTrigger;

export interface PopoverProps extends Omit<AriaPopoverProps, "className"> {
  className?: string;
}

/** Floating surface for menus, selects, and pickers. */
export function Popover({ className, offset = 4, ...props }: PopoverProps) {
  return <AriaPopover {...props} offset={offset} className={cx("ui-popover", className)} />;
}

export interface MenuProps<T> extends Omit<AriaMenuProps<T>, "className"> {
  className?: string;
  /** Root menus default to "bottom start"; submenus (`isSubmenu`) open beside their item. */
  placement?: AriaPopoverProps["placement"];
  /** Set for the menu inside a SubmenuTrigger. */
  isSubmenu?: boolean;
}

/** A menu in its popover. Place directly inside MenuTrigger (or SubmenuTrigger) after the trigger. */
export function Menu<T extends object>({ className, placement, isSubmenu, ...props }: MenuProps<T>) {
  return (
    <Popover {...(isSubmenu ? {} : { placement: placement ?? "bottom start" })}>
      <AriaMenu {...props} className={cx("ui-menu", className)} />
    </Popover>
  );
}

export interface MenuItemProps<T> extends Omit<AriaMenuItemProps<T>, "className" | "children"> {
  children: ReactNode;
  icon?: IconComponent;
  /** Keybinding hint (`Mod+B`); the command still binds through the registry. */
  keybinding?: string;
  description?: string;
  /** Marks a submenu parent. */
  hasSubmenu?: boolean;
  /** Destructive item styling. */
  tone?: "danger";
}

export function MenuItem<T extends object>({
  children,
  icon: Icon,
  keybinding,
  description,
  hasSubmenu,
  tone,
  textValue,
  ...props
}: MenuItemProps<T>) {
  return (
    <AriaMenuItem
      {...props}
      textValue={textValue ?? (typeof children === "string" ? children : undefined)}
      className="ui-menu-item"
      data-tone={tone}
    >
      {({ isSelected, selectionMode }) => (
        <>
          {selectionMode !== "none" ? (
            <span className="ui-menu-item__check">{isSelected && <Check size={14} aria-hidden />}</span>
          ) : (
            Icon && <Icon size={14} aria-hidden className="ui-menu-item__icon" />
          )}
          <span className="ui-menu-item__text">
            <Text slot="label">{children}</Text>
            {description && (
              <Text slot="description" className="ui-menu-item__description">
                {description}
              </Text>
            )}
          </span>
          {keybinding && <Keyboard className="ui-menu-item__keys">{keybindingLabel(keybinding)}</Keyboard>}
          {hasSubmenu && <ChevronRight size={14} aria-hidden className="ui-menu-item__submenu" />}
        </>
      )}
    </AriaMenuItem>
  );
}

export function MenuSection<T extends object>({ title, children }: { title?: string; children: ReactNode }) {
  return (
    <AriaMenuSection<T> className="ui-menu-section">
      {title && <Header className="ui-menu-section__header">{title}</Header>}
      {children}
    </AriaMenuSection>
  );
}

export function MenuSeparator() {
  return <Separator className="ui-menu-separator" />;
}
