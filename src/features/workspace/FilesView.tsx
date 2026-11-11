// Files sidebar: open-folder empty state with recent projects, recovery
// entries from an earlier session, and the lazily loaded, keyboard-accessible
// file tree (react-aria Tree).
import { ChevronRight, File, FileWarning, Folder, FolderOpen, FolderPlus, History, RefreshCw } from "lucide-react";
import type { ReactNode } from "react";
import { Button, Tree, TreeItem, TreeItemContent, type Key } from "react-aria-components";

import type { FileEntry } from "../../bindings/FileEntry";
import type { RecoveryEntry } from "../../bindings/RecoveryEntry";
import {
  ROOT,
  discardRecovery,
  loadDir,
  openFile,
  openFolderDialog,
  openWorkspace,
  restoreRecovery,
  setExpanded,
  useWorkspace,
} from "./store";

function formatTime(ms: number) {
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

function EmptyState() {
  const recent = useWorkspace((s) => s.recent);
  const opening = useWorkspace((s) => s.opening);
  const error = useWorkspace((s) => s.error);
  return (
    <div className="xp-files-empty" data-testid="files-empty">
      <p>No folder is open.</p>
      <Button className="xp-button xp-button-primary" onPress={() => void openFolderDialog()} isDisabled={opening}>
        <FolderPlus size={14} aria-hidden /> Open Folder…
      </Button>
      {error && (
        <p role="alert" className="xp-files-error">
          {error}
        </p>
      )}
      {recent.length > 0 && (
        <section aria-labelledby="xp-recent-title" className="xp-recent">
          <h3 id="xp-recent-title">Recent</h3>
          <ul>
            {recent.map((project) => (
              <li key={project.path}>
                <Button
                  className="xp-recent-item"
                  onPress={() => void openWorkspace(project.path)}
                  aria-label={`Open ${project.name} (${project.path})`}
                >
                  <span className="xp-recent-name">{project.name}</span>
                  <span className="xp-recent-path">{project.path}</span>
                </Button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

function RecoveryList({ entries }: { entries: RecoveryEntry[] }) {
  return (
    <section className="xp-recovery" aria-labelledby="xp-recovery-title" data-testid="recovery-list">
      <h3 id="xp-recovery-title">
        <History size={14} aria-hidden /> Unsaved changes from a previous session
      </h3>
      <ul>
        {entries.map((entry) => (
          <li key={entry.id} data-recovery-path={entry.path}>
            <div className="xp-recovery-path">{entry.path}</div>
            <div className="xp-recovery-meta">
              {formatTime(entry.savedAt)}
              {entry.disk === "changed" && " · file changed on disk since"}
              {entry.disk === "missing" && " · file no longer exists"}
            </div>
            <div className="xp-recovery-actions">
              <Button className="xp-button xp-button-primary" onPress={() => void restoreRecovery(entry)}>
                Restore
              </Button>
              <Button className="xp-button xp-button-danger" onPress={() => void discardRecovery(entry)}>
                Discard
              </Button>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}

function EntryIcon({ entry, expanded }: { entry: FileEntry; expanded: boolean }) {
  if (entry.unavailable) return <FileWarning size={14} aria-hidden />;
  if (entry.kind === "directory") return expanded ? <FolderOpen size={14} aria-hidden /> : <Folder size={14} aria-hidden />;
  return <File size={14} aria-hidden />;
}

function renderEntries(entries: FileEntry[], dirs: ReturnType<typeof useWorkspace.getState>["dirs"]): ReactNode {
  return entries.map((entry) => {
    const isDir = entry.kind === "directory" && !entry.unavailable;
    const children = isDir ? dirs[entry.path] : undefined;
    return (
      <TreeItem
        key={entry.path}
        id={entry.path}
        textValue={entry.name}
        hasChildItems={isDir}
        isDisabled={Boolean(entry.unavailable)}
        className="xp-tree-item"
        data-path={entry.path}
        onAction={isDir || entry.kind !== "file" ? undefined : () => openFile(entry)}
      >
        <TreeItemContent>
          {({ isExpanded }) => (
            <div className="xp-tree-row" title={entry.unavailable ?? entry.path}>
              {isDir ? (
                <Button slot="chevron" className="xp-tree-chevron" aria-label={isExpanded ? "Collapse" : "Expand"}>
                  <ChevronRight size={12} aria-hidden />
                </Button>
              ) : (
                <span className="xp-tree-chevron-spacer" />
              )}
              <EntryIcon entry={entry} expanded={isExpanded} />
              <span className="xp-tree-name">{entry.name}</span>
            </div>
          )}
        </TreeItemContent>
        {isDir && children?.entries && renderEntries(children.entries, dirs)}
        {isDir && children?.loading && !children.entries && (
          <TreeItem id={`${entry.path}/\u0000loading`} textValue="Loading" isDisabled className="xp-tree-item">
            <TreeItemContent>
              <div className="xp-tree-row xp-tree-muted">Loading…</div>
            </TreeItemContent>
          </TreeItem>
        )}
      </TreeItem>
    );
  });
}

export function FilesView() {
  const info = useWorkspace((s) => s.info);
  const dirs = useWorkspace((s) => s.dirs);
  const expanded = useWorkspace((s) => s.expanded);
  const recovery = useWorkspace((s) => s.recovery);
  if (!info) return <EmptyState />;
  const root = dirs[ROOT];
  return (
    <div className="xp-files" data-testid="files-view" data-root={info.root}>
      <div className="xp-files-header">
        <span className="xp-files-title" title={info.root}>
          {info.name}
        </span>
        <Button className="xp-icon-button" aria-label="Refresh" onPress={() => void loadDir(ROOT)}>
          <RefreshCw size={14} aria-hidden />
        </Button>
        <Button className="xp-icon-button" aria-label="Open Folder…" onPress={() => void openFolderDialog()}>
          <FolderPlus size={14} aria-hidden />
        </Button>
      </div>
      {recovery.length > 0 && <RecoveryList entries={recovery} />}
      {root?.error && (
        <p role="alert" className="xp-files-error">
          {root.error}
        </p>
      )}
      <Tree
        aria-label={`Files in ${info.name}`}
        className="xp-tree"
        data-testid="file-tree"
        expandedKeys={expanded}
        onExpandedChange={(keys: Set<Key>) => setExpanded(new Set([...keys].map(String)))}
        renderEmptyState={() => (root?.loading ? "Loading…" : "This folder is empty.")}
      >
        {root?.entries ? renderEntries(root.entries, dirs) : []}
      </Tree>
    </div>
  );
}
