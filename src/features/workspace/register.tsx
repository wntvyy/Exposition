// Workspace contributions: the Files sidebar view and folder commands.
import { Files } from "lucide-react";

import { registry } from "../../app/registry";
import { startDropTarget } from "./dropTarget";
import { activeRenameTarget, openNewFileDialog, openRenameDialog, startImportEvents } from "./explorerActions";
import { FilesView } from "./FilesView";
import { closeWorkspace, FILES_VIEW_ID, openFolderDialog, startWorkspaceEvents, useWorkspace } from "./store";
import "./workspace.css";

startWorkspaceEvents();
// Files dropped from the operating system onto the Files tree are imported (QOL G).
startDropTarget();
startImportEvents();

registry.registerView({
  id: FILES_VIEW_ID,
  location: "sidebar",
  title: "Files",
  icon: Files,
  order: 0,
  render: () => <FilesView />,
});

/** The page bar's project label: the open folder's name (full path as a tooltip), or "No folder open". */
function ProjectLabel() {
  const info = useWorkspace((s) => s.info);
  return (
    <span className="xp-pagebar-project" title={info?.root} data-testid="pagebar-project">
      {info ? info.name : "No folder open"}
    </span>
  );
}

registry.registerStatusItem({
  id: "workspace.project",
  alignment: "pagebar",
  order: 0,
  render: () => <ProjectLabel />,
});

registry.registerCommand({
  id: "workspace.openFolder",
  title: "Open Folder…",
  category: "File",
  keybinding: "Mod+O",
  run: openFolderDialog,
});

registry.registerCommand({
  id: "workspace.closeFolder",
  title: "Close Folder",
  category: "File",
  enabled: () => useWorkspace.getState().info !== null,
  run: closeWorkspace,
});

// QOL E: New File (in the Files tree's selected folder, else the project
// folder) and Rename of the item selected in the focused explorer. F2 acts
// only while an explorer item has focus ("workbench" scope: never in Monaco,
// whose F2 renames a symbol, or in a text field).
registry.registerCommand({
  id: "files.newFile",
  title: "New File…",
  category: "File",
  keybinding: "Mod+N",
  enabled: () => useWorkspace.getState().info !== null,
  run: () => openNewFileDialog(),
});

registry.registerCommand({
  id: "files.rename",
  title: "Rename…",
  category: "File",
  keybinding: "F2",
  keybindingScope: "workbench",
  enabled: () => activeRenameTarget() !== null,
  run: () => {
    const found = activeRenameTarget();
    if (found) openRenameDialog(found.target, found.surface);
  },
});
