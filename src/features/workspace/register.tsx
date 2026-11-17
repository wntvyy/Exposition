// Workspace contributions: the Files sidebar view, folder commands, and the
// page bar's project menu.
import { FilePlus2, Files, FolderInput, FolderOpen, FolderX, History } from "lucide-react";

import { registry } from "../../app/registry";
import { Button } from "../../ui/components/Button";
import { Menu, MenuItem, MenuSeparator, MenuTrigger, SubmenuTrigger } from "../../ui/components/Menu";
import { startDropTarget } from "./dropTarget";
import { activeRenameTarget, openNewFileDialog, openRenameDialog, startImportEvents } from "./explorerActions";
import { FilesView } from "./FilesView";
import {
  closeWorkspace,
  FILES_VIEW_ID,
  openFolderDialog,
  openWorkspace,
  samePath,
  startWorkspaceEvents,
  useWorkspace,
} from "./store";
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

const RECENT_PREFIX = "recent:";

/**
 * The page bar's project label: the open folder's name (full path as a
 * tooltip), or "No folder open". It opens the project menu: Open Folder…,
 * Open Recent, New Project…, Close Folder — the same commands as the palette,
 * available while a project is open (opening another one switches projects).
 */
function ProjectMenu() {
  const info = useWorkspace((s) => s.info);
  const recent = useWorkspace((s) => s.recent).filter((p) => !info || !samePath(p.path, info.root));
  const canCreate = Boolean(registry.command("projects.new"));
  return (
    <MenuTrigger>
      <Button variant="ghost" icon={FolderOpen} className="xp-pagebar-project" data-testid="pagebar-project">
        <span title={info?.root}>{info ? info.name : "No folder open"}</span>
      </Button>
      <Menu
        aria-label="Project"
        onAction={(key) => {
          const id = String(key);
          // Recent entries act in their submenu (below).
          if (!id.startsWith(RECENT_PREFIX)) void registry.runCommand(id);
        }}
      >
        <MenuItem id="workspace.openFolder" icon={FolderInput} keybinding="Mod+O">
          Open Folder…
        </MenuItem>
        {recent.length > 0 && (
          <SubmenuTrigger>
            <MenuItem id="recent" icon={History} hasSubmenu>
              Open Recent
            </MenuItem>
            <Menu
              isSubmenu
              aria-label="Open Recent"
              onAction={(key) => void openWorkspace(String(key).slice(RECENT_PREFIX.length))}
            >
              {recent.slice(0, 10).map((project) => (
                <MenuItem
                  key={project.path}
                  id={`${RECENT_PREFIX}${project.path}`}
                  description={project.path}
                  textValue={project.name}
                >
                  {project.name}
                </MenuItem>
              ))}
            </Menu>
          </SubmenuTrigger>
        )}
        {canCreate && (
          <MenuItem id="projects.new" icon={FilePlus2}>
            New Project…
          </MenuItem>
        )}
        {info && <MenuSeparator />}
        {info && (
          <MenuItem id="workspace.closeFolder" icon={FolderX}>
            Close Folder
          </MenuItem>
        )}
      </Menu>
    </MenuTrigger>
  );
}

registry.registerStatusItem({
  id: "workspace.project",
  alignment: "pagebar",
  order: 0,
  render: () => <ProjectMenu />,
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
