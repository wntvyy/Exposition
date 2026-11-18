// V2 review samples: the Editor workbench (page bar, activity rail, Files,
// editor tabs with a static code sample, Problems, status bar). Mock data only.
import {
  AlertTriangle,
  Bot,
  ChevronDown,
  ChevronRight,
  ClipboardPaste,
  Copy,
  CopyPlus,
  FilePlus,
  FlaskConical,
  Files,
  FolderPlus,
  Hammer,
  ListTree,
  MoreHorizontal,
  Network,
  PanelBottom,
  PanelLeft,
  PanelRight,
  Pencil,
  RefreshCw,
  Search,
  Settings,
  Sigma,
  Trash2,
  X,
  XCircle,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";

import type { ThemeKind } from "./fileIcons";
import { tokenizeLine } from "./highlight";
import { Btn, ChromeLabel, FileGlyph, Kbd } from "./kit";
import { EDITOR_TREE, PROBLEMS, PROJECT_NAME, SQUIGGLES, TABS, type EditorTab, type TreeNode } from "./mock";

// ---- Page bar and rail

export function PageBar({ page = "Editor" }: { page?: string }) {
  const pages: [string, LucideIcon][] = [
    ["Editor", Pencil],
    ["Research", FlaskConical],
    ["Agents", Bot],
    ["Graph", Network],
  ];
  return (
    <header className="v2-pagebar">
      <span className="v2-pagebar__project" title={PROJECT_NAME}>
        <Files size={14} aria-hidden />
        {PROJECT_NAME}
      </span>
      <nav className="v2-pagebar__tabs" aria-label="Pages">
        {pages.map(([label, Icon]) => (
          <button key={label} type="button" className="v2-pagebar__tab" aria-current={label === page ? "page" : undefined}>
            <Icon size={14} aria-hidden />
            {label}
          </button>
        ))}
      </nav>
      <span className="v2-spacer" />
      <Btn variant="ghost" size="sm" icon={Settings} label="Manage" />
    </header>
  );
}

function ActivityRail() {
  const top: [string, LucideIcon, boolean][] = [
    ["Files", Files, true],
    ["Search", Search, false],
    ["Outline", ListTree, false],
    ["Mathematics", Sigma, false],
  ];
  return (
    <nav className="v2-rail" aria-label="Editor views">
      {top.map(([label, Icon, active]) => (
        <button key={label} type="button" className="v2-rail__btn" aria-pressed={active} title={label} aria-label={label}>
          <Icon size={20} aria-hidden />
        </button>
      ))}
      <span className="v2-spacer" />
      {(
        [
          ["Toggle primary sidebar", PanelLeft],
          ["Toggle panel", PanelBottom],
          ["Toggle secondary sidebar", PanelRight],
        ] as [string, LucideIcon][]
      ).map(([label, Icon]) => (
        <button key={label} type="button" className="v2-rail__btn v2-rail__btn--layout" title={label} aria-label={label}>
          <Icon size={18} aria-hidden />
        </button>
      ))}
    </nav>
  );
}

// ---- Files

interface FlatRow {
  node: TreeNode;
  path: string;
  depth: number;
}

function flatten(nodes: TreeNode[], expanded: Set<string>, prefix = "", depth = 0, out: FlatRow[] = []): FlatRow[] {
  const sorted = [...nodes].sort((a, b) => (a.kind === b.kind ? 0 : a.kind === "folder" ? -1 : 1));
  for (const node of sorted) {
    const path = prefix ? `${prefix}/${node.name}` : node.name;
    out.push({ node, path, depth });
    if (node.kind === "folder" && expanded.has(path) && node.children) flatten(node.children, expanded, path, depth + 1, out);
  }
  return out;
}

export function initialExpanded(nodes: TreeNode[], prefix = "", out = new Set<string>()): Set<string> {
  for (const node of nodes) {
    const path = prefix ? `${prefix}/${node.name}` : node.name;
    if (node.kind === "folder" && node.expanded) out.add(path);
    if (node.children) initialExpanded(node.children, path, out);
  }
  return out;
}

export type RowState = "hover" | "selected" | "focused" | "disabled";

export function FileTree({
  tree,
  theme,
  iconSize = 14,
  selected,
  dirty,
  onSelect,
  menuFor,
  onMenu,
  forced,
  label = "Files",
}: {
  tree: TreeNode[];
  theme: ThemeKind;
  iconSize?: 14 | 16;
  selected?: string;
  dirty?: Set<string>;
  onSelect?: (path: string) => void;
  menuFor?: string | null;
  onMenu?: (path: string | null) => void;
  /** Palette sheets: force row states by path. */
  forced?: Record<string, RowState>;
  label?: string;
}) {
  const [expanded, setExpanded] = useState(() => initialExpanded(tree));
  const rows = flatten(tree, expanded);
  const listRef = useRef<HTMLDivElement>(null);
  const [menuAt, setMenuAt] = useState({ top: 0, left: 0 });

  // The menu is positioned against the sample frame (not the scrolling tree),
  // so it can extend past the sidebar like a real popover.
  useLayoutEffect(() => {
    if (!menuFor || !listRef.current) return;
    const row = listRef.current.querySelector<HTMLElement>(`[data-path="${CSS.escape(menuFor)}"]`);
    const frame = listRef.current.closest<HTMLElement>(".v2s-frame");
    if (!row || !frame) return;
    const r = row.getBoundingClientRect();
    const f = frame.getBoundingClientRect();
    setMenuAt({ top: r.bottom - f.top - 2, left: r.left - f.left + 64 });
  }, [menuFor]);

  useEffect(() => {
    if (!menuFor) return;
    const close = (e: KeyboardEvent) => {
      if (e.key === "Escape") onMenu?.(null);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [menuFor, onMenu]);

  return (
    <div className="v2-tree" role="tree" aria-label={label} ref={listRef}>
      {rows.map(({ node, path, depth }) => {
        const isFolder = node.kind === "folder";
        const open = isFolder && expanded.has(path);
        const state = forced?.[path];
        return (
          <div
            key={path}
            role="treeitem"
            aria-level={depth + 1}
            aria-expanded={isFolder ? open : undefined}
            aria-selected={selected === path || state === "selected" || state === "focused"}
            aria-disabled={state === "disabled" || undefined}
            tabIndex={selected === path ? 0 : -1}
            className="v2-tree__row"
            data-path={path}
            data-state={state}
            data-current={selected === path ? "true" : undefined}
            data-menu={menuFor === path ? "true" : undefined}
            style={{ paddingInlineStart: `calc(var(--v2-inset) + ${depth * 12}px)` }}
            onClick={() => {
              if (isFolder) {
                setExpanded((prev) => {
                  const next = new Set(prev);
                  if (next.has(path)) next.delete(path);
                  else next.add(path);
                  return next;
                });
              } else onSelect?.(path);
              onMenu?.(null);
            }}
            onContextMenu={(e) => {
              if (!onMenu) return;
              e.preventDefault();
              onMenu(path);
            }}
          >
            <span className="v2-tree__twisty" aria-hidden>
              {isFolder ? open ? <ChevronDown size={14} /> : <ChevronRight size={14} /> : null}
            </span>
            <FileGlyph path={path} kind={node.kind} theme={theme} size={iconSize} expanded={open} />
            <span className="v2-tree__name" title={node.name}>
              {node.name}
            </span>
            {dirty?.has(path) && <span className="v2-dirty-dot" aria-label="unsaved changes" role="img" />}
          </div>
        );
      })}
      {menuFor && (
        <div className="v2-menu" role="menu" aria-label={`Actions for ${menuFor}`} style={{ top: menuAt.top, left: menuAt.left }} data-testid="v2-files-menu">
          <MenuItem icon={Copy} keys={["Ctrl", "C"]}>
            Copy
          </MenuItem>
          <MenuItem icon={ClipboardPaste} keys={["Ctrl", "V"]} disabled>
            Paste
          </MenuItem>
          <MenuItem icon={CopyPlus}>Duplicate</MenuItem>
          <div className="v2-menu__sep" role="separator" />
          <MenuItem icon={FilePlus}>New File…</MenuItem>
          <MenuItem icon={FolderPlus}>New Folder…</MenuItem>
          <MenuItem icon={Pencil} keys={["F2"]} active>
            Rename…
          </MenuItem>
          <div className="v2-menu__sep" role="separator" />
          <MenuItem icon={Trash2} keys={["Del"]} description="Moves to the Recycle Bin">
            Delete
          </MenuItem>
          <MenuItem icon={X} keys={["Shift", "Del"]} danger description="Cannot be restored">
            Delete Permanently
          </MenuItem>
        </div>
      )}
    </div>
  );
}

export function MenuItem({
  children,
  icon: Icon,
  keys,
  disabled,
  danger,
  active,
  description,
}: {
  children: ReactNode;
  icon?: LucideIcon;
  keys?: string[];
  disabled?: boolean;
  danger?: boolean;
  active?: boolean;
  description?: string;
}) {
  return (
    <div
      role="menuitem"
      className="v2-menu__item"
      aria-disabled={disabled || undefined}
      data-danger={danger ? "true" : undefined}
      data-active={active ? "true" : undefined}
      tabIndex={-1}
    >
      <span className="v2-menu__icon">{Icon && <Icon size={14} aria-hidden />}</span>
      <span className="v2-menu__label">
        {children}
        {description && <span className="v2-menu__desc">{description}</span>}
      </span>
      {keys && (
        <span className="v2-menu__keys">
          {keys.map((k) => (
            <Kbd key={k}>{k}</Kbd>
          ))}
        </span>
      )}
    </div>
  );
}

function FilesSidebar({ theme, menuFor, onMenu, selected, onSelect }: { theme: ThemeKind; menuFor: string | null; onMenu: (p: string | null) => void; selected: string; onSelect: (p: string) => void }) {
  return (
    <aside className="v2-sidebar" aria-label="Files">
      <div className="v2-pane-header">
        <ChromeLabel>Files</ChromeLabel>
        <span className="v2-spacer" />
        <Btn variant="ghost" size="sm" icon={X} label="Close Files" />
      </div>
      <div className="v2-sidebar__project">
        <span className="v2-sidebar__project-name">{PROJECT_NAME}</span>
        <span className="v2-spacer" />
        <Btn variant="ghost" size="sm" icon={FilePlus} label="New File…" />
        <Btn variant="ghost" size="sm" icon={FolderPlus} label="New Folder…" />
        <Btn variant="ghost" size="sm" icon={RefreshCw} label="Refresh" />
      </div>
      <div className="v2-sidebar__body">
        <FileTree tree={EDITOR_TREE} theme={theme} selected={selected} dirty={new Set(["sections/results.typ"])} onSelect={onSelect} menuFor={menuFor} onMenu={onMenu} />
      </div>
      <div className="v2-sidebar__collapsed">
        <ChevronRight size={14} aria-hidden />
        <ChromeLabel>Research</ChromeLabel>
      </div>
    </aside>
  );
}

// ---- Editor

export function TabStrip({
  tabs,
  active,
  onSelect,
  theme,
}: {
  tabs: { id: string; title: string; path?: string; dirty?: boolean; icon?: ReactNode }[];
  active: string;
  onSelect: (id: string) => void;
  theme: ThemeKind;
}) {
  return (
    <div className="v2-tabs" role="tablist" aria-label="Open editors">
      {tabs.map((t) => (
        <div
          key={t.id}
          role="tab"
          tabIndex={t.id === active ? 0 : -1}
          aria-selected={t.id === active}
          aria-label={`${t.title}${t.dirty ? ", unsaved changes" : ""}`}
          className="v2-tab"
          title={t.path}
          onClick={() => onSelect(t.id)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") onSelect(t.id);
          }}
        >
          {t.icon ?? (t.path ? <FileGlyph path={t.path} theme={theme} /> : null)}
          <span className="v2-tab__title">{t.title}</span>
          {t.dirty ? <span className="v2-dirty-dot" aria-hidden /> : <X size={14} aria-hidden className="v2-tab__close" />}
        </div>
      ))}
      <span className="v2-tabs__fill" />
    </div>
  );
}

function CodeView({ tab }: { tab: EditorTab }) {
  const lines = tab.code.split("\n");
  const squiggles = SQUIGGLES[tab.id] ?? [];
  return (
    <div className="v2-code" role="region" aria-label={`${tab.path} (sample, read-only)`} tabIndex={0}>
      <div className="v2-code__lines">
        {lines.map((line, i) => {
          const n = i + 1;
          const sq = squiggles.find((s) => s.line === n);
          return (
            <div key={n} className="v2-code__line" data-current={n === 16 && tab.id === "results" ? "true" : undefined}>
              <span className="v2-code__gutter">{n}</span>
              <span className="v2-code__text">{sq ? <Squiggled line={line} lang={tab.lang} sq={sq} /> : <Tokens line={line} lang={tab.lang} />}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Tokens({ line, lang }: { line: string; lang: EditorTab["lang"] }) {
  return (
    <>
      {tokenizeLine(line, lang).map((t, i) => (
        <span key={i} className={`v2-tok v2-tok--${t.kind}`}>
          {t.text}
        </span>
      ))}
    </>
  );
}

function Squiggled({ line, lang, sq }: { line: string; lang: EditorTab["lang"]; sq: { col: number; length: number; severity: "error" | "warning" } }) {
  const a = sq.col - 1;
  const b = a + sq.length;
  return (
    <>
      <Tokens line={line.slice(0, a)} lang={lang} />
      <span className="v2-squiggle" data-severity={sq.severity}>
        <Tokens line={line.slice(a, b)} lang={lang} />
      </span>
      <Tokens line={line.slice(b)} lang={lang} />
    </>
  );
}

function Breadcrumbs({ path, extra }: { path: string; extra?: ReactNode }) {
  const parts = path.split("/");
  return (
    <div className="v2-crumbs">
      {parts.map((p, i) => (
        <span key={i} className="v2-crumbs__part" data-last={i === parts.length - 1 ? "true" : undefined}>
          {i > 0 && <ChevronRight size={12} aria-hidden />}
          {p}
        </span>
      ))}
      <span className="v2-spacer" />
      {extra}
    </div>
  );
}

function ProblemsPanel({ theme }: { theme: ThemeKind }) {
  const byFile = new Map<string, typeof PROBLEMS>();
  for (const p of PROBLEMS) byFile.set(p.path, [...(byFile.get(p.path) ?? []), p]);
  return (
    <section className="v2-panel" aria-label="Panel">
      <div className="v2-panel__tabs" role="tablist" aria-label="Panel views">
        {[
          ["Problems", true, PROBLEMS.length],
          ["Research", false, null],
          ["Jobs", false, null],
          ["Toolchains", false, null],
        ].map(([label, sel, count]) => (
          <span key={String(label)} role="tab" aria-selected={Boolean(sel)} className="v2-panel__tab">
            <ChromeLabel>{label as string}</ChromeLabel>
            {count ? <span className="v2-count">{count as number}</span> : null}
          </span>
        ))}
        <span className="v2-spacer" />
        <Btn variant="ghost" size="sm" icon={X} label="Close panel" />
      </div>
      <div className="v2-problems" role="list" aria-label="Problems">
        {[...byFile.entries()].map(([path, items]) => (
          <div key={path} className="v2-problems__group">
            <div className="v2-problems__file">
              <ChevronDown size={14} aria-hidden />
              <FileGlyph path={path} theme={theme} />
              <span className="v2-problems__name">{path.slice(path.lastIndexOf("/") + 1)}</span>
              <span className="v2-meta">{path.slice(0, path.lastIndexOf("/"))}</span>
              <span className="v2-count">{items.length}</span>
            </div>
            {items.map((p) => (
              <div key={p.message} role="listitem" className="v2-problems__row" data-severity={p.severity}>
                {p.severity === "error" ? <XCircle size={14} aria-label="Error" /> : <AlertTriangle size={14} aria-label="Warning" />}
                <span className="v2-problems__msg">{p.message}</span>
                <span className="v2-meta">
                  {p.source} · Ln {p.line}, Col {p.col}
                </span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </section>
  );
}

function StatusBar({ agentWaiting }: { agentWaiting: boolean }) {
  return (
    <footer className="v2-statusbar">
      <span className="v2-statusbar__item">Ln 16, Col 27</span>
      <span className="v2-statusbar__item">UTF-8 · LF · typst</span>
      <span className="v2-statusbar__chip" data-tone="danger">
        <XCircle size={12} aria-hidden />
        Build failed: 1 error
      </span>
      <span className="v2-spacer" />
      {agentWaiting && (
        <span className="v2-statusbar__chip" data-tone="warning">
          <AlertTriangle size={12} aria-hidden />
          Agent: waiting for approval
        </span>
      )}
      <span className="v2-statusbar__item">Exposition 0.1.0</span>
    </footer>
  );
}

// ---- Workbench

export interface WorkbenchProps {
  theme: ThemeKind;
  /** Initial active tab id (editor) and context-menu path. */
  initialTab?: string;
  initialMenu?: string | null;
  /** Replaces the editor tab content (Settings). */
  settings?: ReactNode;
  /** The auxiliary sidebar (the Agent panel). */
  auxiliary?: ReactNode;
  auxiliaryWidth?: number;
}

export function Workbench({ theme, initialTab = "results", initialMenu = null, settings, auxiliary, auxiliaryWidth = 400 }: WorkbenchProps) {
  const [active, setActive] = useState(settings ? "settings" : initialTab);
  const [menuFor, setMenuFor] = useState<string | null>(initialMenu);
  const tab = TABS.find((t) => t.id === active);
  const tabs = [
    ...(settings ? [{ id: "settings", title: "Settings", icon: <Settings size={14} aria-hidden className="v2-glyph--chrome" /> }] : []),
    ...TABS.map((t) => ({ id: t.id, title: t.path.slice(t.path.lastIndexOf("/") + 1), path: t.path, dirty: t.dirty })),
  ];
  const selectedFile = tab?.path ?? "sections/results.typ";
  return (
    <div className="v2-workbench" style={{ gridTemplateColumns: `auto var(--v2-sidebar) minmax(0, 1fr) ${auxiliary ? `${auxiliaryWidth}px` : ""}` }}>
      <PageBar />
      <ActivityRail />
      <FilesSidebar
        theme={theme}
        menuFor={menuFor}
        onMenu={setMenuFor}
        selected={selectedFile}
        onSelect={(path) => {
          const t = TABS.find((x) => x.path === path);
          if (t) setActive(t.id);
        }}
      />
      <main className="v2-editor-area">
        <TabStrip tabs={tabs} active={active} onSelect={setActive} theme={theme} />
        {active === "settings" && settings ? (
          <div className="v2-editor-area__settings">{settings}</div>
        ) : tab ? (
          <>
            <Breadcrumbs
              path={tab.path}
              extra={
                <>
                  <Btn variant="ghost" size="sm" icon={Hammer}>
                    Build
                  </Btn>
                  <Btn variant="ghost" size="sm" icon={MoreHorizontal} label="More actions" />
                </>
              }
            />
            <CodeView tab={tab} />
          </>
        ) : null}
        {active !== "settings" && <ProblemsPanel theme={theme} />}
      </main>
      {auxiliary && <aside className="v2-aux">{auxiliary}</aside>}
      <StatusBar agentWaiting={Boolean(auxiliary)} />
    </div>
  );
}

