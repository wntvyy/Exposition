import { X } from "lucide-react";
import type { KeyboardEvent, ReactNode } from "react";
import {
  Tab as AriaTab,
  TabList as AriaTabList,
  TabPanel as AriaTabPanel,
  Tabs as AriaTabs,
  type Key,
  type TabListProps as AriaTabListProps,
  type TabPanelProps as AriaTabPanelProps,
  type TabProps as AriaTabProps,
  type TabsProps as AriaTabsProps,
} from "react-aria-components";

import type { IconComponent } from "./Button";
import { cx } from "./cx";
import { keybindingLabel } from "./Kbd";

/** Tabs container. Arrow keys move between tabs; selection follows focus. */
export function Tabs({ className, ...props }: Omit<AriaTabsProps, "className"> & { className?: string }) {
  return <AriaTabs {...props} className={cx("ui-tabs", className)} />;
}

export function TabList<T extends object>({
  className,
  ...props
}: Omit<AriaTabListProps<T>, "className"> & { className?: string; "aria-label": string }) {
  return <AriaTabList {...props} className={cx("ui-tab-list", className)} />;
}

export function Tab({
  className,
  icon: Icon,
  children,
  ...props
}: Omit<AriaTabProps, "className" | "children"> & { className?: string; icon?: IconComponent; children: ReactNode }) {
  return (
    <AriaTab {...props} className={cx("ui-tab", className)}>
      {Icon && <Icon size={14} aria-hidden />}
      <span className="ui-tab__label">{children}</span>
    </AriaTab>
  );
}

export function TabPanel({ className, ...props }: Omit<AriaTabPanelProps, "className"> & { className?: string }) {
  return <AriaTabPanel {...props} className={cx("ui-tab-panel", className)} />;
}

export interface EditorTabItem {
  key: string;
  title: string;
  /** Unsaved changes: shows a dot (replaced by the close button on hover/focus). */
  dirty?: boolean;
  /** Preview tabs are replaced by the next preview open; shown in italics. */
  preview?: boolean;
  /** Full path or description, shown as a native tooltip. */
  description?: string;
  icon?: IconComponent;
}

export interface EditorTabListProps {
  "aria-label": string;
  items: EditorTabItem[];
  /** Close request (close button, middle click, or Delete on the focused tab). */
  onClose: (key: string) => void;
  /** Double-click: e.g. pin a preview tab. */
  onDoubleClick?: (key: string) => void;
  className?: string;
}

/**
 * Closable document tabs for an editor group. Place inside `Tabs` with
 * `selectedKey`. Close via the close affordance, middle click, Delete on the
 * focused tab, or the Close Editor command (Mod+W).
 */
export function EditorTabList({ items, onClose, onDoubleClick, className, ...aria }: EditorTabListProps) {
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Delete") return;
    const tab = (event.target as HTMLElement).closest<HTMLElement>("[data-editor-key]");
    const key = tab?.dataset.editorKey;
    if (key) {
      event.preventDefault();
      onClose(key);
    }
  };
  return (
    <div className={cx("ui-editor-tabs", className)} onKeyDown={onKeyDown}>
      <AriaTabList aria-label={aria["aria-label"]} className="ui-editor-tabs__list" items={items}>
        {(item) => (
          <AriaTab
            id={item.key}
            className="ui-editor-tab"
            aria-label={`${item.title}${item.dirty ? ", unsaved changes" : ""}${item.preview ? ", preview" : ""}`}
            data-editor-key={item.key}
            data-dirty={item.dirty || undefined}
            data-preview={item.preview || undefined}
            onMouseDown={(e) => {
              if (e.button === 1) e.preventDefault(); // no autoscroll on middle click
            }}
            onAuxClick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                onClose(item.key);
              }
            }}
            onDoubleClick={() => onDoubleClick?.(item.key)}
          >
            {item.icon && <item.icon size={14} aria-hidden className="ui-editor-tab__icon" />}
            <span className="ui-editor-tab__title" title={item.description}>
              {item.title}
            </span>
            <span
              className="ui-editor-tab__close"
              aria-hidden
              title={`Close (${keybindingLabel("Mod+W")})`}
              onPointerDown={(e) => e.stopPropagation()}
              onMouseDown={(e) => e.stopPropagation()}
              onClick={(e) => {
                e.stopPropagation();
                onClose(item.key);
              }}
            >
              <span className="ui-editor-tab__dirty-dot" />
              <X size={14} className="ui-editor-tab__x" />
            </span>
          </AriaTab>
        )}
      </AriaTabList>
    </div>
  );
}

export type { Key };
