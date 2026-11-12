// Editor: source-editor preferences, saved in the application settings
// (workspace service `settings_update`) and applied live by open editors.
import { useEffect } from "react";

import type { EditorSettings } from "../../bindings/EditorSettings";
import { notify } from "../../app/notifications";
import { errorMessage } from "../../lib/ipc";
import { LoadingState, Select, Switch } from "../../ui/components";
import { loadAppSettings, updateAppSettings, useAppSettings } from "../editor/settings";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";

const FONT_SIZES = [10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24];
const TAB_SIZES = [2, 3, 4, 8];

/** Size options, keeping a saved value that is not in the list. */
export function numberOptions(values: number[], current: number, unit = "") {
  const all = values.includes(current) ? values : [...values, current].sort((a, b) => a - b);
  return all.map((v) => ({ id: v, label: `${v}${unit}` }));
}

async function change(patch: Partial<EditorSettings>) {
  const { settings } = useAppSettings.getState();
  try {
    await updateAppSettings({ ...settings, editor: { ...settings.editor, ...patch } });
  } catch (e) {
    notify({ tone: "danger", title: "Could not save editor settings", body: errorMessage(e) });
  }
}

export function EditorSection() {
  const loaded = useAppSettings((s) => s.loaded);
  const editor = useAppSettings((s) => s.settings.editor);
  useEffect(() => {
    void loadAppSettings();
  }, []);

  return (
    <SettingsPage title="Editor" description="Source editor preferences. Open editors update immediately." testId="settings-editor-prefs">
      {!loaded ? (
        <LoadingState title="Loading settings…" />
      ) : (
        <>
          <SettingsGroup title="Text">
            <SettingRow label="Font size">
              {({ labelId }) => (
                <Select
                  aria-labelledby={labelId}
                  value={editor.fontSize}
                  options={numberOptions(FONT_SIZES, editor.fontSize, " px")}
                  onChange={(fontSize) => void change({ fontSize })}
                />
              )}
            </SettingRow>
            <SettingRow label="Wrap long lines">
              {({ labelId }) => (
                <Switch aria-labelledby={labelId} isSelected={editor.wordWrap} onChange={(wordWrap) => void change({ wordWrap })} />
              )}
            </SettingRow>
            <SettingRow label="Line numbers">
              {({ labelId }) => (
                <Switch
                  aria-labelledby={labelId}
                  isSelected={editor.lineNumbers}
                  onChange={(lineNumbers) => void change({ lineNumbers })}
                />
              )}
            </SettingRow>
            <SettingRow label="Minimap" description="Overview of the document beside the scroll bar.">
              {({ labelId, descriptionId }) => (
                <Switch
                  aria-labelledby={labelId}
                  aria-describedby={descriptionId}
                  isSelected={editor.minimap}
                  onChange={(minimap) => void change({ minimap })}
                />
              )}
            </SettingRow>
          </SettingsGroup>
          <SettingsGroup title="Indentation and line endings">
            <SettingRow label="Tab size">
              {({ labelId }) => (
                <Select
                  aria-labelledby={labelId}
                  value={editor.tabSize}
                  options={numberOptions(TAB_SIZES, editor.tabSize, " spaces")}
                  onChange={(tabSize) => void change({ tabSize })}
                />
              )}
            </SettingRow>
            <SettingRow label="Indent with spaces" description="Off inserts tab characters.">
              {({ labelId, descriptionId }) => (
                <Switch
                  aria-labelledby={labelId}
                  aria-describedby={descriptionId}
                  isSelected={editor.insertSpaces}
                  onChange={(insertSpaces) => void change({ insertSpaces })}
                />
              )}
            </SettingRow>
            <SettingRow
              label="Line ending for new lines"
              description="Used in files that have no line endings yet; other files keep the endings they already use."
            >
              {({ labelId, descriptionId }) => (
                <Select
                  aria-labelledby={labelId}
                  aria-describedby={descriptionId}
                  value={editor.defaultEol}
                  options={[
                    { id: "lf", label: "LF (\\n)" },
                    { id: "crlf", label: "CRLF (\\r\\n)" },
                  ]}
                  onChange={(defaultEol) => void change({ defaultEol })}
                />
              )}
            </SettingRow>
          </SettingsGroup>
        </>
      )}
    </SettingsPage>
  );
}
