// Application settings cache (source editor preferences, startup behavior).
// The first load also reports a settings file that was invalid and set aside.
import { create } from "zustand";

import type { AppSettings } from "../../bindings/AppSettings";
import { commands } from "../../lib/ipc";
import { reportSettingsWarning } from "../settings/warnings";

const defaults: AppSettings = {
  version: 1,
  editor: {
    fontSize: 13,
    tabSize: 2,
    insertSpaces: true,
    wordWrap: true,
    lineNumbers: true,
    minimap: false,
    defaultEol: "lf",
  },
  files: { reopenLastProject: false },
};

export const useAppSettings = create<{ settings: AppSettings; loaded: boolean }>(() => ({
  settings: defaults,
  loaded: false,
}));

let loading: Promise<void> | null = null;

export function loadAppSettings(): Promise<void> {
  loading ??= commands.settingsGet().then(
    (state) => {
      useAppSettings.setState({ settings: state.settings, loaded: true });
      // The service sets an invalid file aside on the first read, so only
      // this read can report it.
      reportSettingsWarning("app", state.warning);
    },
    () => useAppSettings.setState({ loaded: true }),
  );
  return loading;
}

export async function updateAppSettings(settings: AppSettings): Promise<void> {
  const saved = await commands.settingsUpdate(settings);
  useAppSettings.setState({ settings: saved, loaded: true });
}

/** Test hook: forget the cached load. */
export function resetAppSettingsForTests(): void {
  loading = null;
  useAppSettings.setState({ settings: defaults, loaded: false });
}
