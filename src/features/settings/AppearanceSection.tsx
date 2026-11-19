// Appearance: the color theme (the shared theme store; persisted per profile).
// Four built-in themes plus "Follow system" (Light or Dark by the OS setting).
import { ChoiceCards, ThemePreview } from "../../ui/components";
import { setThemePreference, THEME_OPTIONS, useTheme, type ThemePreference } from "../../ui/theme";
import { themeLabels } from "../../ui/tokens";
import { SettingsGroup, SettingsPage } from "./parts";

const OPTIONS = THEME_OPTIONS.map((option) => ({ ...option, preview: <ThemePreview theme={option.id} /> }));

export function AppearanceSection() {
  const preference = useTheme((s) => s.preference);
  const resolved = useTheme((s) => s.resolved);
  return (
    <SettingsPage title="Appearance" testId="settings-appearance">
      <SettingsGroup
        title="Color theme"
        description={
          preference === "system"
            ? `Follows your system setting (currently ${themeLabels[resolved]}).`
            : "Applies immediately. Open documents, drafts, and graph positions stay as they are."
        }
      >
        <ChoiceCards<ThemePreference> aria-label="Color theme" value={preference} onChange={setThemePreference} options={OPTIONS} />
      </SettingsGroup>
    </SettingsPage>
  );
}
