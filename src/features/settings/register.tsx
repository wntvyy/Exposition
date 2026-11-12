// Settings feature registration: the Settings editor (`settings:` resource)
// and the commands that open it.
import { editors } from "../../app/editors";
import { registry } from "../../app/registry";
import { SETTINGS_RESOURCE, type SectionId } from "./model";
import { SettingsEditor } from "./SettingsEditor";
import { showSection } from "./store";

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
