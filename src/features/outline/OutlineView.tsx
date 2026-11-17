// Editor "Outline" view (auxiliary panel): the active document's sections and
// mathematical items (theorems, lemmas, definitions, proofs, Lean
// declarations) in source order, from its current text — unsaved edits
// included and labelled — plus the compiled pages of its last successful
// build. Rows navigate; nothing here edits the document or the graph, and
// rows are not graph entities (nesting is layout, not dependence).
import { BookOpen, Braces, CornerDownRight, FileText, Hash, Sigma } from "lucide-react";
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";

import { useActiveEditor } from "../../app/editors";
import { registry } from "../../app/registry";
import type { DocumentOutline } from "../../bindings/DocumentOutline";
import type { OutlineItem } from "../../bindings/OutlineItem";
import {
  Badge,
  Button,
  EmptyState,
  ErrorState,
  LoadingState,
  Section,
  StatusText,
  Tree,
  TreeItem,
  type IconComponent,
} from "../../ui/components";
import { openDocument, type DocumentHandle } from "../editor";
import { coverageStateLabel } from "../graph/status";
import { useTypesetting } from "../typesetting/store";
import { useWorkspace } from "../workspace/store";
import { flatten, itemName, pageList, type OutlineTarget } from "./model";
import { navigateToItem, showPage } from "./navigate";
import { outlineStore } from "./store";
import "./outline.css";

/** Files the outline reads (others say there is no outline). */
const OUTLINE_FILE = /\.(tex|ltx|typ|md|markdown|lean)$/i;
/** Debounce after edits before the outline is recomputed. */
const REFRESH_DELAY_MS = 300;

const noop = () => () => {};
const zero = () => 0;

function itemIcon(item: OutlineItem): IconComponent {
  if (item.kind === "heading") return Hash;
  switch (item.entityKind) {
    case "definition":
    case "assumption":
      return BookOpen;
    case "proof":
      return CornerDownRight;
    case "formalDeclaration":
      return Braces;
    default:
      return Sigma;
  }
}

function renderItems(items: OutlineItem[]) {
  return items.map((item) => (
    <TreeItem
      key={item.id}
      id={item.id}
      title={itemName(item)}
      textValue={itemName(item)}
      icon={itemIcon(item)}
      hasChildItems={item.children.length > 0}
      trailing={item.label ? <span className="xp-outline__label">{item.label}</span> : undefined}
      aria-description={item.kind === "heading" ? item.typeLabel : undefined}
      data-outline-id={item.id}
      data-kind={item.kind}
      data-type={item.typeLabel}
    >
      {renderItems(item.children)}
    </TreeItem>
  ));
}

function CoverageNote({ outline }: { outline: DocumentOutline }) {
  return (
    <p className="ui-meta xp-outline__note" data-testid="outline-coverage" data-state={outline.coverage}>
      Extraction: {coverageStateLabel(outline.coverage)}
      {outline.extractor ? ` (${outline.extractor})` : ""}
      {outline.detail ? ` — ${outline.detail}` : ""}
    </p>
  );
}

function SourceOutline({ outline }: { outline: DocumentOutline }) {
  const all = useMemo(() => flatten(outline.items), [outline]);
  // Collapsed rows are remembered by key while the document stays the same.
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  useEffect(() => setCollapsed(new Set()), [outline.path]);
  const expanded = useMemo(
    () => new Set(all.filter((i) => i.children.length > 0 && !collapsed.has(i.id)).map((i) => i.id)),
    [all, collapsed],
  );
  const byId = useMemo(() => new Map(all.map((i) => [i.id, i])), [all]);
  if (outline.items.length === 0) {
    return (
      <p className="ui-muted xp-outline__empty" data-testid="outline-empty">
        No sections or theorem-like items found.
      </p>
    );
  }
  return (
    <Tree
      aria-label={`Outline of ${outline.path}`}
      className="xp-tree xp-outline__tree"
      data-testid="outline-tree"
      expandedKeys={expanded}
      onExpandedChange={(keys) => {
        const open = new Set([...keys].map(String));
        setCollapsed(new Set(all.filter((i) => i.children.length > 0 && !open.has(i.id)).map((i) => i.id)));
      }}
      onAction={(key) => {
        const item = byId.get(String(key));
        if (item) void navigateToItem(item, outline, outlineStore);
      }}
    >
      {renderItems(outline.items)}
    </Tree>
  );
}

function PageList({ path, handle }: { path: string; handle: DocumentHandle }) {
  const entry = useTypesetting((s) => s.entries[path]);
  const list = pageList(entry, handle.id, handle.sync.revision);
  const status =
    list.state === "failed" ? (
      <StatusText status="stale" label="Showing pages from an earlier revision" wrap data-testid="outline-pages-status" />
    ) : list.state === "earlier" ? (
      <StatusText status="stale" label="Earlier revision" detail={`built from revision ${list.buildRevision}`} data-testid="outline-pages-status" />
    ) : list.building ? (
      <StatusText status="running" label="Building" data-testid="outline-pages-status" />
    ) : null;
  return (
    <Section
      variant="content"
      title="Pages"
      count={list.pages.length || undefined}
      collapsible
      data-testid="outline-pages"
      data-state={list.state}
      data-build={list.build?.id ?? ""}
      data-build-revision={list.buildRevision ?? ""}
    >
      {status && <div className="xp-outline__status">{status}</div>}
      {list.pages.length === 0 ? (
        <EmptyState
          title="No pages built yet"
          actions={
            <Button size="sm" variant="secondary" onPress={() => void showPage(path, 0)}>
              Show Page view
            </Button>
          }
        />
      ) : (
        <Tree
          aria-label="Compiled pages"
          className="xp-tree xp-outline__tree"
          data-testid="outline-page-list"
          onAction={(key) => void showPage(path, Number(String(key).slice("page:".length)))}
        >
          {list.pages.map((p) => (
            <TreeItem key={p.index} id={`page:${p.index}`} title={`Page ${p.index + 1}`} icon={FileText} data-page={p.index} />
          ))}
        </Tree>
      )}
    </Section>
  );
}

/** Follow the active document: target, handle, and debounced refreshes. */
function useOutlineFor(path: string | null, root: string | null) {
  const [handle, setHandle] = useState<{ path: string; handle: DocumentHandle } | null>(null);
  const [openError, setOpenError] = useState<string | null>(null);
  const supported = path !== null && OUTLINE_FILE.test(path);

  // The active tab's document is open already; this joins its handle.
  useEffect(() => {
    let cancelled = false;
    setOpenError(null);
    if (!path || !supported) {
      setHandle(null);
      return;
    }
    openDocument(path).then(
      (h) => !cancelled && setHandle({ path, handle: h }),
      (e: unknown) => !cancelled && setOpenError(String((e as { message?: string })?.message ?? e)),
    );
    return () => {
      cancelled = true;
    };
  }, [path, supported]);

  const current = handle && handle.path === path && !handle.handle.closed ? handle.handle : null;
  const target: OutlineTarget | null = current && path ? { documentId: current.id, path, root } : null;
  const targetKey = target ? `${target.documentId}\u0000${target.path}\u0000${target.root}` : "";

  useEffect(() => {
    outlineStore.setTarget(target);
    if (!current || !target) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    void outlineStore.refresh();
    const off = current.subscribe(() => {
      clearTimeout(timer);
      timer = setTimeout(() => void outlineStore.refresh(), REFRESH_DELAY_MS);
    });
    return () => {
      off();
      clearTimeout(timer);
    };
    // `targetKey` identifies the target (document, path, project).
  }, [targetKey]);

  // Re-render on document changes (revision, saved state).
  useSyncExternalStore(current?.subscribe ?? noop, current?.getVersion ?? zero);
  return { handle: current, supported, openError };
}

export function OutlineView() {
  const folder = useWorkspace((s) => s.info);
  const active = useActiveEditor();
  const path = active?.resource.path ?? null;
  const { handle, supported, openError } = useOutlineFor(path, folder?.root ?? null);
  const state = useSyncExternalStore(outlineStore.subscribe, outlineStore.getState);

  if (!folder) return <EmptyState title="No folder open" description="Open a project folder, then a document to see its outline." />;
  if (!active || !path) {
    return <EmptyState title="No document active" description="Open a .tex, .typ, or .md file to see its sections and theorems." />;
  }
  if (!supported) {
    return (
      <div className="xp-outline" data-testid="outline-view" data-path={path} data-coverage="unsupported">
        <EmptyState title="No outline for this kind of file" description="Outlines cover LaTeX, Typst, Markdown, and Lean files." />
      </div>
    );
  }
  if (openError) return <ErrorState title="Could not read this document" description={openError} live={false} />;
  const outline = state.outline && state.outline.path === path && handle ? state.outline : null;
  const typeset = (registry.editorFor(active.resource)?.modes ?? []).includes("page");
  return (
    <div
      className="xp-outline"
      data-testid="outline-view"
      data-path={path}
      data-revision={outline?.revision ?? ""}
      data-coverage={outline?.coverage ?? ""}
      data-count={outline?.itemCount ?? ""}
      aria-busy={state.loading}
    >
      <header className="xp-outline__header">
        <span className="ui-code xp-outline__path" title={path}>
          {path}
        </span>
        {outline?.unsaved && (
          <Badge tone="info" dashed title="Read from the unsaved editor buffer, not the saved file" data-testid="outline-unsaved">
            Unsaved buffer (revision {outline.revision})
          </Badge>
        )}
      </header>
      {!outline && state.error && (
        <ErrorState
          title="Could not read the outline"
          description={state.error}
          live={false}
          actions={
            <Button size="sm" onPress={() => void outlineStore.refresh()}>
              Retry
            </Button>
          }
        />
      )}
      {!outline && !state.error && <LoadingState title="Reading outline…" />}
      {outline && (
        <>
          <CoverageNote outline={outline} />
          <Section variant="content" title="Source" count={outline.itemCount || undefined} collapsible data-testid="outline-source">
            <SourceOutline outline={outline} />
          </Section>
        </>
      )}
      {typeset && handle && <PageList path={path} handle={handle} />}
    </div>
  );
}
