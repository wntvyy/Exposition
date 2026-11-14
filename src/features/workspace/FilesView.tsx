// Files sidebar: open-folder empty state with recent projects, recovery
// entries from an earlier session, and the lazily loaded, keyboard-accessible
// file tree (shared Tree component).
import { File, FilePlus2, FileWarning, Folder, FolderOpen, FolderPlus, Link2, RefreshCw, Unlink, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type MouseEvent, type ReactNode } from "react";

import type { FileEntry } from "../../bindings/FileEntry";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import type { RecoveryEntry } from "../../bindings/RecoveryEntry";
import { OpContextMenu } from "../operations/OpMenu";
import { useOpSurface } from "../operations/surfaces";
import { ResearchFilesSection } from "../research/FilesSection";
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
  useContextMenu,
} from "../../ui/components";
import { fileActions, type FileSelectionItem } from "./fileOps";
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

let lastPointerToggle = 0;

/** Toggle a folder; a double click (pointer toggle, then the row action) toggles once. */
function toggleFolder(path: string, via: "pointer" | "keyboard") {
  if (via === "keyboard" && Date.now() - lastPointerToggle < 500) return;
  if (via === "pointer") lastPointerToggle = Date.now();
  const expanded = new Set(useWorkspace.getState().expanded);
  if (expanded.has(path)) expanded.delete(path);
  else expanded.add(path);
  setExpanded(expanded);
}

/** Every listed entry by path (from the loaded folders). */
function entryIndex(dirs: Record<string, DirState>): Map<string, FileEntry> {
  const index = new Map<string, FileEntry>();
  for (const dir of Object.values(dirs)) for (const entry of dir.entries ?? []) index.set(entry.path, entry);
  return index;
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
        data-kind={entry.kind}
        aria-description={entry.unavailable ?? undefined}
        onAction={
          entry.kind === "file" && !entry.unavailable ? () => openFile(entry) : isDir ? () => toggleFolder(entry.path, "keyboard") : undefined
        }
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

/**
 * The file tree with multi-selection and the operation commands (Copy, Paste,
 * Duplicate, Delete, Delete Permanently) for its focused selection, by
 * shortcut, context menu, or the command palette.
 */
function FileTree({ name, rootPath, entries }: { name: string; rootPath: string; entries: FileEntry[] }) {
  const dirs = useWorkspace((s) => s.dirs);
  const expanded = useWorkspace((s) => s.expanded);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const ref = useRef<HTMLDivElement>(null);
  const menu = useContextMenu();
  const index = useMemo(() => entryIndex(dirs), [dirs]);

  // Forget selected paths that are gone (deleted, renamed, folder closed).
  useEffect(() => {
    if ([...selected].some((p) => !index.has(p))) setSelected(new Set([...selected].filter((p) => index.has(p))));
  }, [index, selected]);

  const items: FileSelectionItem[] = [...selected].flatMap((path) => {
    const entry = index.get(path);
    return entry && !entry.unavailable ? [{ path, kind: entry.kind === "directory" ? ("directory" as const) : ("file" as const), name: entry.name }] : [];
  });
  const actions = () =>
    fileActions(items, {
      root: rootPath,
      afterCreate: (paths, folder) => {
        if (folder) setExpanded(new Set([...useWorkspace.getState().expanded, folder]));
        setSelected(new Set(paths));
      },
    });
  useOpSurface("files", ref, actions);

  const pathAt = (target: EventTarget | null) =>
    target instanceof Element ? (target.closest("[data-path]")?.getAttribute("data-path") ?? null) : null;

  // A plain click opens a file or toggles a folder; react-aria handles selection.
  const onClickCapture = (event: MouseEvent) => {
    if (event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    if (event.target instanceof Element && event.target.closest(".ui-tree-item__chevron")) return;
    const path = pathAt(event.target);
    const entry = path ? index.get(path) : undefined;
    if (!entry || entry.unavailable) return;
    if (entry.kind === "file") openFile(entry);
    else if (event.detail <= 1) toggleFolder(entry.path, "pointer");
  };

  // Right click on an unselected item selects it alone; on a selected one keeps the selection.
  const onContextMenu = (event: MouseEvent) => {
    const path = pathAt(event.target);
    const entry = path ? index.get(path) : undefined;
    if (entry && !entry.unavailable && !selected.has(entry.path)) setSelected(new Set([entry.path]));
    else if (!entry && event.button === 2) setSelected(new Set());
    menu.openFromEvent(event);
  };

  return (
    <div ref={ref} className="xp-ops-surface" onClickCapture={onClickCapture} onContextMenu={onContextMenu} data-testid="files-surface">
      <Tree
        aria-label={`Files in ${name}`}
        className="xp-tree"
        data-testid="file-tree"
        selectionMode="multiple"
        selectionBehavior="replace"
        selectedKeys={selected}
        onSelectionChange={(keys) =>
          setSelected(keys === "all" ? new Set([...index.keys()]) : new Set([...keys].map(String).filter((k) => index.has(k))))
        }
        expandedKeys={expanded}
        onExpandedChange={(keys) => setExpanded(new Set([...keys].map(String)))}
        renderEmptyState={() => <EmptyState fill={false} title="This folder is empty" />}
      >
        {renderEntries(entries, dirs, expanded)}
      </Tree>
      {menu.position && <OpContextMenu controller={menu} label="File actions" actions={actions()} testId="files-context-menu" />}
    </div>
  );
}

export function FilesView() {
  const info = useWorkspace((s) => s.info);
  const dirs = useWorkspace((s) => s.dirs);
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
        <FileTree name={info.name} rootPath={info.root} entries={root.entries} />
      )}
      <ResearchFilesSection />
    </div>
  );
}
