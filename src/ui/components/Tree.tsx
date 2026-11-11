import { ChevronRight } from "lucide-react";
import type { CSSProperties, ReactNode } from "react";
import {
  Button as AriaButton,
  Tree as AriaTree,
  TreeItem as AriaTreeItem,
  TreeItemContent,
  type TreeItemProps as AriaTreeItemProps,
  type TreeProps as AriaTreeProps,
} from "react-aria-components";

import type { IconComponent } from "./Button";
import { cx } from "./cx";

export interface TreeProps<T> extends Omit<AriaTreeProps<T>, "className"> {
  "aria-label": string;
  className?: string;
}

/**
 * Hierarchical list (file tree, outline). Arrow keys navigate, Right/Left
 * expand/collapse, Enter performs the item action, type-ahead finds items.
 */
export function Tree<T extends object>({ className, ...props }: TreeProps<T>) {
  return <AriaTree {...props} className={cx("ui-tree", className)} />;
}

export interface TreeItemProps extends Omit<AriaTreeItemProps, "className" | "children" | "textValue"> {
  title: string;
  icon?: IconComponent;
  /** Trailing content: badges, StatusPill, counts. */
  trailing?: ReactNode;
  /** Nested TreeItems. */
  children?: ReactNode;
  textValue?: string;
}

export function TreeItem({ title, icon: Icon, trailing, children, textValue, ...props }: TreeItemProps) {
  return (
    <AriaTreeItem
      {...props}
      textValue={textValue ?? title}
      className="ui-tree-item"
      style={({ level }) => ({ "--tree-level": level - 1 }) as CSSProperties}
    >
      <TreeItemContent>
        {({ hasChildItems }) => (
          <div className="ui-tree-item__row">
            {hasChildItems ? (
              <AriaButton slot="chevron" className="ui-tree-item__chevron">
                <ChevronRight size={14} aria-hidden />
              </AriaButton>
            ) : (
              <span className="ui-tree-item__chevron-spacer" />
            )}
            {Icon && <Icon size={14} aria-hidden className="ui-tree-item__icon" />}
            <span className="ui-tree-item__title">{title}</span>
            {trailing && <span className="ui-tree-item__trailing">{trailing}</span>}
          </div>
        )}
      </TreeItemContent>
      {children}
    </AriaTreeItem>
  );
}
