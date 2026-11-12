// Built-in workbench contributions: layout/navigation/theme commands, the UI
// reference editor, and the application status item.
import { lazy, Suspense, useEffect, useState } from "react";

import type { AppInfo } from "../../bindings/AppInfo";
import { commands as ipc } from "../../lib/ipc";
import { LoadingState } from "../../ui/components/States";
import { setThemePreference, toggleTheme } from "../../ui/theme";
import { editors, useEditors, type EditorGroupId } from "../editors";
import { defaultLayout, layout, useLayout } from "../layout";
import { registry, type CommandContribution, type EditorResource } from "../registry";
import { commandPalette, SHOW_COMMANDS_ID } from "./CommandPalette";
import { focusNextRegion } from "./regions";

export const UI_REFERENCE: EditorResource = { key: "ui-reference:", title: "UI Reference" };

const UiReference = lazy(() => import("../../ui/reference/UiReference").then((m) => ({ default: m.UiReference })));

function UiReferenceEditor() {
  return (
    <Suspense fallback={<LoadingState title="Loading UI reference…" />}>
      <UiReference />
    </Suspense>
  );
}

function AppStatusItem() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  useEffect(() => {
    let alive = true;
    ipc.appInfo().then(
      (value) => alive && setInfo(value),
      () => {},
    );
    return () => {
      alive = false;
    };
  }, []);
  if (!info) return null;
  return (
    <span className="wb-statusbar__text" data-testid="app-info">
      {info.name} {info.version}
      {info.debugBuild ? " (debug)" : ""}
    </span>
  );
}

function cycleEditor(step: 1 | -1) {
  const { groups, activeGroup } = useEditors.getState();
  const group = groups[activeGroup];
  const index = group.editors.findIndex((e) => e.resource.key === group.activeKey);
  if (index < 0 || group.editors.length < 2) return;
  const next = group.editors[(index + step + group.editors.length) % group.editors.length]!;
  editors.activate(next.resource.key, activeGroup);
}

const hasActiveEditor = () => editors.active() !== undefined;

const builtinCommands: CommandContribution[] = [
  {
    id: SHOW_COMMANDS_ID,
    title: "Show All Commands",
    category: "View",
    keybinding: "Mod+Shift+P",
    run: () => commandPalette.toggle(),
  },
  {
    id: "workbench.action.toggleSidebar",
    title: "Toggle Sidebar",
    category: "View",
    keybinding: "Mod+B",
    run: () => layout.toggle("sidebar"),
  },
  {
    id: "workbench.action.togglePanel",
    title: "Toggle Panel",
    category: "View",
    keybinding: "Mod+J",
    run: () => layout.toggle("panel"),
  },
  {
    id: "workbench.action.toggleAuxiliary",
    title: "Toggle Auxiliary Panel",
    category: "View",
    keybinding: "Mod+Alt+B",
    run: () => layout.toggle("auxiliary"),
  },
  {
    id: "workbench.action.focusNextRegion",
    title: "Focus Next Region",
    category: "View",
    keybinding: "F6",
    run: () => focusNextRegion(false),
  },
  {
    id: "workbench.action.focusPreviousRegion",
    title: "Focus Previous Region",
    category: "View",
    keybinding: "Shift+F6",
    run: () => focusNextRegion(true),
  },
  {
    id: "workbench.action.closeEditor",
    title: "Close Editor",
    category: "View",
    keybinding: "Mod+W",
    enabled: hasActiveEditor,
    run: async () => {
      const { activeGroup, groups } = useEditors.getState();
      const key = groups[activeGroup].activeKey;
      if (key) await editors.closeInGroup(key, activeGroup);
    },
  },
  {
    id: "workbench.action.splitEditor",
    title: "Split Editor Right",
    category: "View",
    keybinding: "Mod+\\",
    enabled: hasActiveEditor,
    run: () => {
      const active = editors.active();
      if (active) editors.open(active.resource, { group: "secondary", mode: active.mode });
    },
  },
  {
    id: "workbench.action.closeEditorGroup",
    title: "Close Right Editor Group",
    category: "View",
    enabled: () => useEditors.getState().groups.secondary.editors.length > 0,
    run: async () => {
      await editors.closeGroup("secondary" satisfies EditorGroupId);
    },
  },
  {
    id: "workbench.action.nextEditor",
    title: "Open Next Editor",
    category: "View",
    keybinding: "Mod+PageDown",
    enabled: hasActiveEditor,
    run: () => cycleEditor(1),
  },
  {
    id: "workbench.action.previousEditor",
    title: "Open Previous Editor",
    category: "View",
    keybinding: "Mod+PageUp",
    enabled: hasActiveEditor,
    run: () => cycleEditor(-1),
  },
  {
    id: "workbench.action.resetLayout",
    title: "Reset Layout",
    category: "View",
    run: () => {
      for (const region of ["sidebar", "panel", "auxiliary"] as const) {
        layout.setSize(region, defaultLayout[region].size);
        layout.setVisible(region, true);
      }
      layout.setEditorSplit(defaultLayout.editorSplit);
      useLayout.setState({ forced: {} });
    },
  },
  {
    id: "workbench.action.toggleTheme",
    title: "Toggle Light/Dark Theme",
    category: "Preferences",
    run: () => toggleTheme(),
  },
  {
    id: "workbench.action.theme.light",
    title: "Color Theme: Light",
    category: "Preferences",
    run: () => setThemePreference("light"),
  },
  {
    id: "workbench.action.theme.dark",
    title: "Color Theme: Dark",
    category: "Preferences",
    run: () => setThemePreference("dark"),
  },
  {
    id: "workbench.action.theme.system",
    title: "Color Theme: Follow System",
    category: "Preferences",
    run: () => setThemePreference("system"),
  },
  {
    id: "workbench.action.showUiReference",
    title: "Show UI Reference",
    category: "Developer",
    run: () => editors.open(UI_REFERENCE),
  },
];

/** Register built-in contributions. Returns a disposer (safe under StrictMode double effects). */
export function registerBuiltins(): () => void {
  const disposers = [
    ...builtinCommands.map((command) => registry.registerCommand(command)),
    registry.registerEditor({
      id: "workbench.uiReference",
      matches: (resource) => resource.key === UI_REFERENCE.key,
      modes: ["source"],
      component: UiReferenceEditor,
    }),
    registry.registerStatusItem({ id: "workbench.status.app", alignment: "right", order: 1000, render: () => <AppStatusItem /> }),
  ];
  return () => {
    for (const dispose of disposers) dispose();
  };
}
