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
