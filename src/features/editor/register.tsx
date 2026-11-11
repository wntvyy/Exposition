// Source editor contributions: the Monaco editor for workspace text files,
// save commands, and the encoding/line-ending status item.
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useSyncExternalStore } from "react";

import { editors, useActiveEditor } from "../../app/editors";
import { registry, type EditorProps } from "../../app/registry";
import { commands } from "../../lib/ipc";
import { ask } from "../workspace/prompt";
import { activeHandle, handleForPath, openHandles, saveAll, saveDocument } from "./documentClient";
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

// Closing the window with unsaved changes asks first. Unsaved buffers are
// journaled for crash recovery either way; "Don't Save" discards them.
function installWindowCloseGuard() {
  let current;
  try {
    current = getCurrentWindow();
  } catch {
    return; // Not running inside the desktop shell (e.g. unit tests).
  }
  void current
    .onCloseRequested(async (event) => {
      await Promise.all(openHandles().map((h) => h.sync.idle()));
      const dirty = (await commands.documentList()).filter((d) => d.path && d.savedRevision !== d.revision);
      if (dirty.length === 0) return;
      const names = dirty.map((d) => d.path?.split("/").pop()).join(", ");
      const choice = await ask({
        title: dirty.length === 1 ? `Save changes to ${names}?` : `Save changes to ${dirty.length} files?`,
        message:
          dirty.length === 1
            ? "Your changes will be lost if you don't save them."
            : `${names} have unsaved changes. They will be lost if you don't save them.`,
        buttons: [
          { id: "save", label: dirty.length === 1 ? "Save" : "Save All", primary: true },
          { id: "discard", label: "Don't Save", danger: true },
          { id: "cancel", label: "Cancel", cancel: true },
        ],
      });
      if (choice === "save") {
        if (!(await saveAll())) event.preventDefault();
      } else if (choice === "discard") {
        for (const doc of dirty) await commands.documentClose(doc.id).catch(() => {});
      } else {
        event.preventDefault();
      }
    })
    .catch(() => {});
}

installWindowCloseGuard();
