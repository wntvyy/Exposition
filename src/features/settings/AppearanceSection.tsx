// Appearance: the color theme (the shared theme store; persisted per profile).
import { Monitor, Moon, Sun } from "lucide-react";

import { SegmentedControl } from "../../ui/components";
import { setThemePreference, useTheme, type ThemePreference } from "../../ui/theme";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";

const THEME_OPTIONS: { id: ThemePreference; label: string; icon: typeof Sun }[] = [
  { id: "light", label: "Light", icon: Sun },
  { id: "dark", label: "Dark", icon: Moon },
  { id: "system", label: "System", icon: Monitor },
];

export function AppearanceSection() {
  const preference = useTheme((s) => s.preference);
  const resolved = useTheme((s) => s.resolved);
  return (
    <SettingsPage title="Appearance" testId="settings-appearance">
      <SettingsGroup title="Theme">
        <SettingRow
          label="Color theme"
          description={
            preference === "system"
              ? `Follows your system setting (currently ${resolved}).`
              : "Applies to the whole workbench, including source editors."
          }
        >
          <SegmentedControl
            aria-label="Color theme"
            value={preference}
            onChange={setThemePreference}
            options={THEME_OPTIONS}
            size="md"
          />
        </SettingRow>
      </SettingsGroup>
    </SettingsPage>
  );
}
