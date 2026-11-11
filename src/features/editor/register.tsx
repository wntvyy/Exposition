// Source editor contributions: the Monaco editor for workspace text files,
// save commands, and the encoding/line-ending status item.
import { useSyncExternalStore } from "react";

import { editors, useActiveEditor } from "../../app/editors";
import { registry, type EditorProps } from "../../app/registry";
import { activeHandle, handleForPath, saveAll, saveDocument } from "./documentClient";
import { SourceEditor } from "./SourceEditor";

function SourceEditorTab({ resource, active }: EditorProps) {
  return <SourceEditor path={resource.path} active={active} />;
}

const ENCODING_LABELS: Record<string, string> = {
  utf8: "UTF-8",
  utf8Bom: "UTF-8 with BOM",
  utf16Le: "UTF-16 LE",
  utf16Be: "UTF-16 BE",
};

const EOL_LABELS: Record<string, string> = { lf: "LF", crlf: "CRLF", none: "LF", mixed: "Mixed EOL" };

const noop = () => () => {};
const zero = () => 0;

function DocumentStatus() {
  const active = useActiveEditor();
  const handle = active?.resource.path ? handleForPath(active.resource.path) : undefined;
  useSyncExternalStore(handle?.subscribe ?? noop, handle?.getVersion ?? zero);
  if (!handle) return null;
  return (
    <span className="xp-document-status" data-testid="document-status">
      {ENCODING_LABELS[handle.info.encoding] ?? handle.info.encoding} · {EOL_LABELS[handle.eolMode]} · {handle.info.language}
    </span>
  );
}

registry.registerEditor({
  id: "editor.source",
  matches: (resource) => resource.key.startsWith("file:") && Boolean(resource.path),
  modes: ["source"],
  component: SourceEditorTab,
});

registry.registerCommand({
  id: "file.save",
  title: "Save",
  category: "File",
  keybinding: "Mod+S",
  enabled: () => Boolean(activeHandle()),
  run: async () => {
    const handle = activeHandle();
    if (handle) await saveDocument(handle);
  },
});

registry.registerCommand({
  id: "file.saveAll",
  title: "Save All",
  category: "File",
  keybinding: "Mod+Alt+S",
  run: async () => {
    await saveAll();
  },
});

registry.registerCommand({
  id: "file.close",
  title: "Close Editor",
  category: "File",
  keybinding: "Mod+W",
  enabled: () => Boolean(editors.active()),
  run: async () => {
    const active = editors.active();
    if (active) await editors.close(active.resource.key);
  },
});

registry.registerStatusItem({ id: "editor.documentStatus", alignment: "right", order: 50, render: () => <DocumentStatus /> });
