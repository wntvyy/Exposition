// Menu items for operation actions, shared by every surface's context menu
// and "More actions" menus: same labels, active bindings shown, unavailable
// actions disabled with a concise reason, and recoverable deletion kept apart
// from permanent removal.
import { ClipboardPaste, Copy, CopyPlus, Trash2, X } from "lucide-react";
import type { ReactNode } from "react";

import { ContextMenu, MenuItem, MenuSection, MenuSeparator, type ContextMenuController, type IconComponent } from "../../ui/components";
import { keybindingFor, type SurfaceAction, type SurfaceActionId, type SurfaceActions } from "./surfaces";

const ORDER: { id: SurfaceActionId; label: string; icon: IconComponent; danger?: boolean }[] = [
  { id: "copy", label: "Copy", icon: Copy },
  { id: "paste", label: "Paste", icon: ClipboardPaste },
  { id: "duplicate", label: "Duplicate", icon: CopyPlus },
  { id: "delete", label: "Delete", icon: Trash2 },
  { id: "deletePermanently", label: "Delete Permanently", icon: X, danger: true },
];

export interface ExtraItem {
  id: string;
  label: string;
  icon?: IconComponent;
  description?: string;
  isDisabled?: boolean;
  keybinding?: string;
  run: () => void | Promise<void>;
}

function item(id: string, action: SurfaceAction, fallback: string, icon: IconComponent, danger?: boolean, keybinding?: string): ReactNode {
  return (
    <MenuItem
      key={id}
      id={id}
      icon={icon}
      keybinding={action.enabled ? keybinding : undefined}
      description={action.enabled ? action.description : action.reason ?? action.description}
      isDisabled={!action.enabled}
      tone={danger ? "danger" : undefined}
      textValue={action.label ?? fallback}
    >
      {action.label ?? fallback}
    </MenuItem>
  );
}

/**
 * Menu items for a surface's actions (sections: clipboard, extras, deletion).
 * `keys: false` omits key hints where the keys do not act (e.g. the graph
 * inspector, which is outside the canvas/table selection).
 */
export function opMenuItems(actions: SurfaceActions, extras: ExtraItem[] = [], options: { keys?: boolean } = {}): ReactNode {
  const keys = options.keys !== false;
  const clipboard = ORDER.slice(0, 3).filter((o) => actions[o.id]);
  const deletion = ORDER.slice(3).filter((o) => actions[o.id]);
  const sections: ReactNode[] = [];
  if (clipboard.length) {
    sections.push(
      <MenuSection key="clipboard">
        {clipboard.map((o) => item(o.id, actions[o.id]!, o.label, o.icon, false, keys ? keybindingFor(o.id) : undefined))}
      </MenuSection>,
    );
  }
  if (extras.length) {
    if (sections.length) sections.push(<MenuSeparator key="sep-extras" />);
    sections.push(
      <MenuSection key="extras">
        {extras.map((e) => (
          <MenuItem key={e.id} id={e.id} icon={e.icon} description={e.description} isDisabled={e.isDisabled} keybinding={e.keybinding} textValue={e.label}>
            {e.label}
          </MenuItem>
        ))}
      </MenuSection>,
    );
  }
  if (deletion.length) {
    if (sections.length) sections.push(<MenuSeparator key="sep-delete" />);
    sections.push(
      <MenuSection key="delete">
        {deletion.map((o) => item(o.id, actions[o.id]!, o.label, o.icon, o.danger, keys ? keybindingFor(o.id) : undefined))}
      </MenuSection>,
    );
  }
  return sections;
}

/** Run the action or extra chosen in an operation menu. */
export function runMenuKey(key: unknown, actions: SurfaceActions, extras: ExtraItem[] = []): void {
  const id = String(key);
  const action = actions[id as SurfaceActionId];
  if (action?.enabled) {
    void action.run();
    return;
  }
  const extra = extras.find((e) => e.id === id);
  if (extra && !extra.isDisabled) void extra.run();
}

/** A context menu for a surface: its operation actions plus surface-specific extras. */
export function OpContextMenu({
  controller,
  label,
  actions,
  extras = [],
  testId,
}: {
  controller: ContextMenuController;
  label: string;
  actions: SurfaceActions;
  extras?: ExtraItem[];
  testId?: string;
}) {
  return (
    <ContextMenu controller={controller} aria-label={label} data-testid={testId ?? "op-context-menu"} onAction={(key) => runMenuKey(key, actions, extras)}>
      {opMenuItems(actions, extras)}
    </ContextMenu>
  );
}
