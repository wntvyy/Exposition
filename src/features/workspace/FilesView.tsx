// Files sidebar: open-folder empty state with recent projects, recovery
// entries from an earlier session, and the lazily loaded, keyboard-accessible
// file tree (shared Tree component).
import { File, FilePlus2, FileWarning, Folder, FolderOpen, FolderPlus, Link2, RefreshCw, Unlink, X } from "lucide-react";
import type { ReactNode } from "react";

import type { FileEntry } from "../../bindings/FileEntry";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import type { RecoveryEntry } from "../../bindings/RecoveryEntry";
import {
  Banner,
  Button,
  EmptyState,
  ErrorState,
  IconButton,
  LoadingState,
  Section,
  Toolbar,
  Tree,
  TreeItem,
} from "../../ui/components";
import {
  ROOT,
  discardRecovery,
  forgetRecent,
  loadDir,
  openFile,
  openFolderDialog,
  openWorkspace,
  restoreRecovery,
  setExpanded,
  useWorkspace,
  type DirState,
} from "./store";

function formatTime(ms: number) {
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

function EmptyFolder() {
  const recent = useWorkspace((s) => s.recent);
  const opening = useWorkspace((s) => s.opening);
  const error = useWorkspace((s) => s.error);
  return (
    <div className="xp-files" data-testid="files-empty">
      <EmptyState
        icon={FolderOpen}
        fill={recent.length === 0}
        title="No folder open"
        description="Open a project folder to browse and edit its files."
        actions={
          <>
            <Button variant="primary" icon={FolderPlus} isPending={opening} onPress={() => void openFolderDialog()}>
              Open Folder…
            </Button>
            {/* Project creation (src/features/projects) contributes this command. */}
            {registry.command("projects.new") && (
              <Button icon={FilePlus2} onPress={() => void registry.runCommand("projects.new")} data-testid="files-new-project">
                New Project…
              </Button>
            )}
          </>
        }
      />
      {error && (
        <Banner tone="danger" title="Could not open folder" live>
          {error}
        </Banner>
      )}
      {recent.length > 0 && (
        <Section title="Recent">
          <ul className="xp-recent">
            {recent.map((project) => (
              <li key={project.path} className="xp-recent__row" data-testid="recent-project" data-path={project.path}>
                <Button
                  variant="ghost"
                  className="xp-recent__item"
                  icon={Folder}
                  aria-label={`Open ${project.name} (${project.path})`}
                  onPress={() => void openWorkspace(project.path)}
                >
                  <span className="xp-recent__name">{project.name}</span>
                  <span className="xp-recent__path">{project.path}</span>
                </Button>
                <IconButton
                  icon={X}
                  size="sm"
                  className="xp-recent__remove"
                  aria-label={`Remove ${project.name} from Recent`}
                  onPress={() => void forgetRecent(project)}
                />
              </li>
            ))}
          </ul>
        </Section>
      )}
    </div>
  );
}

function copyText(text: string) {
  void navigator.clipboard?.writeText(text).then(
    () => notify({ tone: "info", title: "Location copied", body: text }),
    () => notify({ tone: "warning", title: "Could not copy the location", body: text }),
  );
}

function RecoveryItem({ entry }: { entry: RecoveryEntry }) {
  if (entry.problem) {
    return (
      <li data-recovery-id={entry.id} data-recovery-unreadable="true">
        <span className="xp-recovery__path">
          Unreadable recovery data{entry.path ? ` for ${entry.path}` : ""}
        </span>
        <span className="xp-recovery__meta">
          {formatTime(entry.savedAt)} · {entry.problem}
        </span>
        {entry.location && (
          <span className="xp-recovery__location" title={entry.location}>
            Kept at {entry.location}
          </span>
        )}
        <span className="xp-recovery__actions">
          {entry.location && (
            <Button size="sm" variant="secondary" onPress={() => copyText(entry.location ?? "")}>
              Copy Location
            </Button>
          )}
          <Button size="sm" variant="secondary" onPress={() => void discardRecovery(entry)}>
            Discard
          </Button>
        </span>
      </li>
    );
  }
  return (
    <li data-recovery-id={entry.id} data-recovery-path={entry.path ?? undefined}>
      <span className="xp-recovery__path">{entry.path}</span>
      <span className="xp-recovery__meta">
        {formatTime(entry.savedAt)}
        {entry.disk === "changed" && " · file changed on disk since"}
        {entry.disk === "missing" && " · file no longer exists"}
      </span>
      <span className="xp-recovery__actions">
        <Button size="sm" variant="primary" onPress={() => void restoreRecovery(entry)}>
          Restore
        </Button>
        <Button size="sm" variant="secondary" onPress={() => void discardRecovery(entry)}>
          Discard
        </Button>
      </span>
    </li>
  );
}

function RecoveryList({ entries }: { entries: RecoveryEntry[] }) {
  return (
    <Banner tone="warning" title="Unsaved changes from a previous session" className="xp-recovery">
      <ul className="xp-recovery__list" data-testid="recovery-list">
        {entries.map((entry) => (
          <RecoveryItem key={entry.id} entry={entry} />
        ))}
      </ul>
    </Banner>
  );
}

function entryIcon(entry: FileEntry, expanded: boolean) {
  if (entry.unavailable) return entry.link ? Unlink : FileWarning;
  if (entry.kind === "directory") return expanded ? FolderOpen : Folder;
  return File;
}

function renderEntries(entries: FileEntry[], dirs: Record<string, DirState>, expanded: Set<string>): ReactNode {
  return entries.map((entry) => {
    const isDir = entry.kind === "directory" && !entry.unavailable;
    const children = isDir ? dirs[entry.path] : undefined;
    return (
      <TreeItem
        key={entry.path}
        id={entry.path}
        title={entry.name}
        icon={entryIcon(entry, expanded.has(entry.path))}
        trailing={
          entry.unavailable ? (
            <span className="xp-tree-note" title={entry.unavailable}>
              {entry.link ? "outside folder" : "unavailable"}
            </span>
          ) : entry.link ? (
            <Link2 size={12} aria-label="Link" />
          ) : undefined
        }
        hasChildItems={isDir}
        isDisabled={Boolean(entry.unavailable)}
        data-path={entry.path}
        aria-description={entry.unavailable ?? undefined}
        onAction={entry.kind === "file" && !entry.unavailable ? () => openFile(entry) : undefined}
      >
        {isDir && children?.entries && renderEntries(children.entries, dirs, expanded)}
        {isDir && !children?.entries && (
          <TreeItem
            id={`${entry.path}/\u0000status`}
            title={children?.error ? `Could not list: ${children.error}` : "Loading…"}
            isDisabled
          />
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
  if (!info) return <EmptyFolder />;
  const root = dirs[ROOT];
  return (
    <div className="xp-files" data-testid="files-view" data-root={info.root}>
      <div className="xp-files__header">
        <span className="xp-files__name" title={info.root}>
          {info.name}
        </span>
        <Toolbar aria-label="Folder actions">
          <IconButton icon={RefreshCw} size="sm" aria-label="Refresh" onPress={() => void loadDir(ROOT)} />
          <IconButton
            icon={FolderPlus}
            size="sm"
            aria-label="Open Folder…"
            keybinding="Mod+O"
            onPress={() => void openFolderDialog()}
          />
        </Toolbar>
      </div>
      {recovery.length > 0 && <RecoveryList entries={recovery} />}
      {root?.error ? (
        <ErrorState
          title="Could not list this folder"
          description={root.error}
          actions={<Button onPress={() => void loadDir(ROOT)}>Retry</Button>}
        />
      ) : !root?.entries ? (
        <LoadingState title="Loading files…" />
      ) : (
        <Tree
          aria-label={`Files in ${info.name}`}
          className="xp-tree"
          data-testid="file-tree"
          expandedKeys={expanded}
          onExpandedChange={(keys) => setExpanded(new Set([...keys].map(String)))}
          renderEmptyState={() => <EmptyState fill={false} title="This folder is empty" />}
        >
          {renderEntries(root.entries, dirs, expanded)}
        </Tree>
      )}
    </div>
  );
}
