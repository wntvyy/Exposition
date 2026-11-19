// Application theme. Follows the OS color scheme by default ("system": Light or
// Dark); an explicit choice of one of the four built-in themes (Light, Dark,
// Ocean, Sage) is persisted. The resolved theme is applied as
// `<html data-theme="light|dark|ocean|sage">`, which `tokens.css` keys on.
//
// Compatibility: values saved before V2 ("light", "dark", "system") are read
// unchanged. A missing, unreadable, or unknown value falls back to "system"
// and is not rewritten until the user picks a theme.
import { create } from "zustand";

import { parseJson, readStored, writeStored } from "./storage";
import { themeKinds, themeLabels, themeNames, type ThemeName } from "./tokens";

export type ThemePreference = "system" | ThemeName;

export const THEME_STORAGE_KEY = "exposition.theme";

const DARK_QUERY = "(prefers-color-scheme: dark)";

export function parseThemePreference(raw: string | null): ThemePreference {
  const value = parseJson(raw);
  return value === "system" || (themeNames as readonly unknown[]).includes(value) ? (value as ThemePreference) : "system";
}

export function resolveTheme(preference: ThemePreference, systemDark: boolean): ThemeName {
  if (preference === "system") return systemDark ? "dark" : "light";
  return preference;
}

function systemPrefersDark(): boolean {
  try {
    return globalThis.matchMedia?.(DARK_QUERY).matches ?? false;
  } catch {
    return false;
  }
}

export interface ThemeState {
  preference: ThemePreference;
  resolved: ThemeName;
}

const initialPreference = parseThemePreference(readStored(THEME_STORAGE_KEY));

export const useTheme = create<ThemeState>(() => ({
  preference: initialPreference,
  resolved: resolveTheme(initialPreference, systemPrefersDark()),
}));

function apply(resolved: ThemeName) {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.theme = resolved;
}

export function setThemePreference(preference: ThemePreference): void {
  const resolved = resolveTheme(preference, systemPrefersDark());
  useTheme.setState({ preference, resolved });
  apply(resolved);
  writeStored(THEME_STORAGE_KEY, JSON.stringify(preference));
}

/** Switch between Light and Dark by the current theme's kind (an explicit choice). */
export function toggleTheme(): void {
  setThemePreference(themeKinds[useTheme.getState().resolved] === "dark" ? "light" : "dark");
}

let installed = false;

/** Apply the theme to the document and follow OS changes while preference is "system". Idempotent. */
export function installTheme(): void {
  apply(useTheme.getState().resolved);
  if (installed) return;
  installed = true;
  try {
    globalThis.matchMedia?.(DARK_QUERY).addEventListener("change", (event) => {
      const { preference } = useTheme.getState();
      if (preference !== "system") return;
      const resolved = resolveTheme(preference, event.matches);
      useTheme.setState({ resolved });
      apply(resolved);
    });
  } catch {
    // matchMedia unavailable: keep the resolved theme.
  }
}

/** The theme choices every picker offers (Settings, the page-bar menu, commands), in order. */
export const THEME_OPTIONS: readonly { id: ThemePreference; label: string }[] = [
  ...themeNames.map((id) => ({ id, label: themeLabels[id] })),
  { id: "system", label: "Follow system" },
];
