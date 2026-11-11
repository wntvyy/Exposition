// Application settings cache for the source editor.
import { create } from "zustand";

import type { AppSettings } from "../../bindings/AppSettings";
import { commands } from "../../lib/ipc";

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
    (state) => useAppSettings.setState({ settings: state.settings, loaded: true }),
    () => useAppSettings.setState({ loaded: true }),
  );
  return loading;
}

export async function updateAppSettings(settings: AppSettings): Promise<void> {
  const saved = await commands.settingsUpdate(settings);
  useAppSettings.setState({ settings: saved, loaded: true });
}
