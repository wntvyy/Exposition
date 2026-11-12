// Settings files that are invalid when loaded are set aside: the original is
// kept beside it as `<name>.invalid-<millis>` and defaults replace what was
// invalid. The owning service reports that in its `warning`; this tells the
// user once per warning, naming the kept file (the warning text does).
import { notify } from "../../app/notifications";

export type SettingsFile = "app" | "project" | "execution";

const TITLES: Record<SettingsFile, string> = {
  app: "Invalid application settings were reset",
  project: "Invalid project settings were reset",
  execution: "Invalid execution settings were reset",
};

const shown = new Set<string>();

/** Notify about a settings warning, once per distinct warning. */
export function reportSettingsWarning(file: SettingsFile, warning: string | null | undefined): void {
  if (!warning) return;
  const key = `${file}\u0000${warning}`;
  if (shown.has(key)) return;
  shown.add(key);
  notify({ id: `settings-warning-${file}`, tone: "warning", title: TITLES[file], body: warning, timeoutMs: null });
}

/** Test hook: forget which warnings were shown. */
export function resetSettingsWarnings(): void {
  shown.clear();
}
