// Workspace contributions: the Files sidebar view and folder commands.
import { Files } from "lucide-react";

import { registry } from "../../app/registry";
import { FilesView } from "./FilesView";
import { closeWorkspace, FILES_VIEW_ID, openFolderDialog, startWorkspaceEvents, useWorkspace } from "./store";
import "./workspace.css";

startWorkspaceEvents();

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
