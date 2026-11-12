// Settings feature registration: the Settings editor (`settings:` resource),
// the commands that open it, the LaTeX engine switch, and the startup check
// for settings files that were invalid and set aside.
import { editors } from "../../app/editors";
import { registry } from "../../app/registry";
import { loadAppSettings } from "../editor/settings";
import { canSwitchEngine, switchEngine } from "./engine";
import { SETTINGS_RESOURCE, type SectionId } from "./model";
import { SettingsEditor } from "./SettingsEditor";
import { loadExecutionSettings, showSection } from "./store";

export function openSettings(section?: SectionId): void {
  if (section) showSection(section);
  editors.open({ ...SETTINGS_RESOURCE });
}

registry.registerEditor({
  id: "settings.editor",
  priority: 100,
  matches: (resource) => resource.key === SETTINGS_RESOURCE.key,
  modes: ["source"],
  component: SettingsEditor,
});

registry.registerCommand({
  id: "workbench.action.openSettings",
  title: "Open Settings",
  category: "Preferences",
  keybinding: "Mod+,",
  run: () => openSettings(),
});

registry.registerCommand({
  id: "settings.trustedFolders",
  title: "Manage Trusted Folders",
  category: "Preferences",
  run: () => openSettings("trust"),
});

registry.registerCommand({
  id: "settings.execution",
  title: "Restricted Execution and Tool Locations",
  category: "Preferences",
  run: () => openSettings("execution"),
});

registry.registerCommand({
  id: "settings.project",
  title: "Project Settings and LaTeX Targets",
  category: "Preferences",
  run: () => openSettings("project"),
});

registry.registerCommand({
  id: "latex.useLuaLatex",
  title: "Build This File with LuaLaTeX",
  category: "LaTeX",
  enabled: canSwitchEngine,
  run: async () => {
    await switchEngine("luaLatex");
  },
});

registry.registerCommand({
  id: "latex.usePdfLatex",
  title: "Build This File with pdfLaTeX",
  category: "LaTeX",
  enabled: canSwitchEngine,
  run: async () => {
    await switchEngine("pdfLatex");
  },
});

// Settings files that were invalid at startup were set aside; say so now
// rather than when a settings page happens to be opened.
void loadAppSettings();
void loadExecutionSettings();
