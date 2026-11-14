// Source editor contributions: the Monaco editor for workspace text files,
// save commands, and the encoding/line-ending status item.
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useSyncExternalStore } from "react";

import { editors, useActiveEditor, useEditors } from "../../app/editors";
import { registry, type EditorProps } from "../../app/registry";
import { commands } from "../../lib/ipc";
import { setEditorLinkHandler } from "../../lib/monaco";
import { confirm, confirmUnsaved, type UnsavedChoice } from "../../ui/components";
import {
  activeHandle,
  fileKey,
  handleForPath,
  openHandles,
  saveAll,
  saveCopy,
  saveDocument,
  type DocumentHandle,
} from "./documentClient";
import { offerEditorLink } from "./links";
import { SourceEditor } from "./SourceEditor";

// Links in editor hovers are copied, never opened (no navigation, no opener).
setEditorLinkHandler((uri) => void offerEditorLink(uri));

function SourceEditorTab({ resource, active }: EditorProps) {
  // Inactive tabs stay mounted; only the selected tab of the active group takes focus.
  const selected = useEditors((s) => s.groups[s.activeGroup].activeKey === resource.key);
  const onEditor = useCallback(
    (_editor: unknown, handle: DocumentHandle | null) => {
      // The tab's path was an alias (case variant, short name, link inside the
      // folder) of a file open under its canonical path: show one tab for it.
      if (!handle?.path || !resource.path || handle.path === resource.path) return;
      editors.open({ key: fileKey(handle.path), path: handle.path, title: handle.name });
      void editors.close(resource.key);
    },
    [resource.key, resource.path],
  );
  return <SourceEditor path={resource.path} active={active && selected} onEditor={onEditor} />;
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
  id: "file.saveCopy",
  title: "Save a Copy…",
  category: "File",
  enabled: () => Boolean(activeHandle()),
  run: async () => {
    const handle = activeHandle();
    if (handle) await saveCopy(handle);
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
      const choice =
        dirty.length === 1
          ? await confirmUnsaved(names)
          : await confirm<UnsavedChoice>({
              title: `Save changes to ${dirty.length} files?`,
              message: `${names} have unsaved changes. They will be lost if you don't save them.`,
              actions: [
                { id: "cancel", label: "Cancel", variant: "secondary" },
                { id: "discard", label: "Don't Save", variant: "secondary" },
                { id: "save", label: "Save All", variant: "primary" },
              ],
              cancelId: "cancel",
              defaultId: "save",
              tone: "warning",
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
