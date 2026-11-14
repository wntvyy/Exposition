// Settings › Storage & History (P1 spec §4): retained run/task records,
// generated outputs, workflow revisions, and recoverable app-managed
// deletions of the open project, with sizes where measurable. Purge removes
// the selected records/payloads through the shared operation client (one
// decision when the plan names a consequence). Referenced or shared items
// are kept by default: they are never part of "Select all", and including one
// leads to its impact decision. Nothing is removed automatically, and record
// removal is not reported as reclaimed disk space.
import { ArchiveRestore, Link2, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";

import type { OpTarget } from "../../bindings/OpTarget";
import type { StorageCategory } from "../../bindings/StorageCategory";
import type { StorageItem } from "../../bindings/StorageItem";
import type { StorageOverview } from "../../bindings/StorageOverview";
import { errorMessage } from "../../lib/ipc";
import {
  Banner,
  Button,
  Checkbox,
  EmptyState,
  ErrorState,
  LoadingState,
  SimpleTable,
  StatusText,
  type SimpleTableColumn,
} from "../../ui/components";
import { onProjectOp, restoreAndReport, runUserOperation } from "../operations/client";
import { opCommands } from "../operations/ipc";
import { formatBytes, targetKey } from "../operations/model";
import { useWorkspace } from "../workspace/store";
import { SettingsGroup, SettingsPage } from "./parts";

/** A row of a category (the same record can appear as a record and as outputs). */
export interface ScopedItem {
  category: string;
  item: StorageItem;
}

/** Selection key of a row: its category and its target. */
export function rowKey(category: string, item: StorageItem): string {
  return `${category}|${targetKey(item.target)}`;
}

export interface StorageScope {
  items: ScopedItem[];
  /** Sum of measurable sizes. */
  bytes: number;
  /** Selected items whose size is not measurable. */
  unmeasured: number;
  referenced: number;
  active: number;
}

/** The selected scope: what Purge would act on, with honest size accounting. */
export function storageScope(overview: StorageOverview | null, selected: ReadonlySet<string>): StorageScope {
  const items = (overview?.categories ?? []).flatMap((c) => c.items.map((item) => ({ category: c.id, item }))).filter((s) => selected.has(rowKey(s.category, s.item)));
  return {
    items,
    bytes: items.reduce((sum, s) => sum + (s.item.bytes ?? 0), 0),
    unmeasured: items.filter((s) => s.item.bytes === null).length,
    referenced: items.filter((s) => s.item.referenced).length,
    active: items.filter((s) => s.item.active).length,
  };
}

/** Keys "Select all" adds for a category: never referenced/shared items. */
export function selectableByDefault(category: StorageCategory): string[] {
  return category.items.filter((i) => !i.referenced).map((i) => rowKey(category.id, i));
}

/**
 * The targets a purge sends: every selected item in ONE request, so a batch
 * gets one combined decision (records and their outputs are separate targets;
 * a record never takes its outputs along). Duplicates collapse.
 */
export function purgeTargets(items: ScopedItem[]): OpTarget[] {
  const seen = new Set<string>();
  const targets: OpTarget[] = [];
  for (const s of items) {
    const key = targetKey(s.item.target);
    if (seen.has(key)) continue;
    seen.add(key);
    targets.push(s.item.target);
  }
  return targets;
}

function formatDate(ms: number | null): string {
  return ms === null ? "—" : new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/** Referenced/shared (kept by default; a settled fact) and active work (running). */
function ItemState({ item }: { item: StorageItem }) {
  if (!item.referenced && !item.active) return <span className="ui-meta">—</span>;
  return (
    <span className="exp-storage__state">
      {item.referenced && (
        <span className="exp-storage__referenced ui-meta" title="Kept by default; including it asks before anything is removed">
          <Link2 size={14} aria-hidden />
          Referenced
        </span>
      )}
      {item.active && <StatusText status="running" label="Active" />}
    </span>
  );
}

function CategoryGroup({
  category,
  selected,
  onToggle,
  onSelectAll,
}: {
  category: StorageCategory;
  selected: ReadonlySet<string>;
  onToggle: (key: string, on: boolean) => void;
  onSelectAll: (keys: string[], on: boolean) => void;
}) {
  const defaults = selectableByDefault(category);
  const allOn = defaults.length > 0 && defaults.every((k) => selected.has(k));
  const recoverable = category.id === "recoverableDeletions";
  const columns: SimpleTableColumn<StorageItem>[] = [
    {
      id: "include",
      title: "Include",
      hideTitle: true,
      width: "1%",
      render: (item) => {
        const key = rowKey(category.id, item);
        return (
          <Checkbox
            aria-label={`Include ${item.label}${item.referenced ? " (referenced; you will be asked first)" : ""}`}
            isSelected={selected.has(key)}
            onChange={(on) => onToggle(key, on)}
          />
        );
      },
    },
    { id: "item", title: "Item", isRowHeader: true, render: (item) => item.label },
    {
      id: "size",
      title: "Size",
      numeric: true,
      width: "1%",
      render: (item) => <span className="exp-storage__nowrap">{formatBytes(item.bytes)}</span>,
    },
    {
      id: "created",
      title: "Created",
      width: "1%",
      render: (item) => <span className="exp-storage__nowrap">{formatDate(item.createdAtMs)}</span>,
    },
    { id: "state", title: "State", width: "1%", render: (item) => <ItemState item={item} /> },
  ];
  if (recoverable) {
    columns.push({
      id: "actions",
      title: "Actions",
      hideTitle: true,
      width: "1%",
      render: (item) =>
        item.target.kind === "recoverable" && (
          <Button
            size="sm"
            variant="secondary"
            icon={ArchiveRestore}
            data-testid="storage-restore"
            onPress={() =>
              void restoreAndReport([{ token: (item.target as { id: string }).id, target: item.target }], new Map([[targetKey(item.target), item.label]]))
            }
          >
            Restore
          </Button>
        ),
    });
  }
  return (
    <SettingsGroup
      title={category.label}
      count={`${category.items.length}${category.bytes !== null ? ` · ${formatBytes(category.bytes)}` : ""}`}
      description={category.note}
      testId={`storage-category-${category.id}`}
      actions={
        defaults.length > 0 ? (
          <Button size="sm" variant="ghost" onPress={() => onSelectAll(defaults, !allOn)}>
            {allOn ? "Clear selection" : "Select unreferenced"}
          </Button>
        ) : undefined
      }
    >
      {category.items.length === 0 ? (
        <p className="ui-meta">Nothing retained.</p>
      ) : (
        <SimpleTable
          aria-label={category.label}
          className="exp-settings__table"
          columns={columns}
          rows={category.items}
          rowKey={(item) => rowKey(category.id, item)}
          rowProps={(item) => ({
            "data-testid": "storage-item",
            "data-key": rowKey(category.id, item),
            "data-referenced": item.referenced || undefined,
            "data-active": item.active || undefined,
          })}
        />
      )}
    </SettingsGroup>
  );
}

export function StorageSection() {
  const root = useWorkspace((s) => s.info?.root ?? null);
  const [overview, setOverview] = useState<StorageOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [purging, setPurging] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const next = await opCommands.storageOverview();
      setOverview(next);
      setError(null);
      // Keep only selections that still exist.
      const keys = new Set(next.categories.flatMap((c) => c.items.map((i) => rowKey(c.id, i))));
      setSelected((current) => new Set([...current].filter((k) => keys.has(k))));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (root) void load();
    else setOverview(null);
  }, [root, load]);
  useEffect(() => onProjectOp(() => void load()), [load]);

  const scope = useMemo(() => storageScope(overview, selected), [overview, selected]);
  const labels = useMemo(() => new Map(scope.items.map((s) => [targetKey(s.item.target), s.item.label])), [scope]);

  // One request for the whole scope: one combined decision.
  const purge = async () => {
    setPurging(true);
    try {
      const done = await runUserOperation({ kind: "delete", permanent: true }, purgeTargets(scope.items), { labels });
      if (done) setSelected(new Set());
    } finally {
      setPurging(false);
    }
  };

  const toggle = (key: string, on: boolean) =>
    setSelected((current) => {
      const next = new Set(current);
      if (on) next.add(key);
      else next.delete(key);
      return next;
    });
  const selectMany = (keys: string[], on: boolean) =>
    setSelected((current) => {
      const next = new Set(current);
      for (const k of keys) {
        if (on) next.add(k);
        else next.delete(k);
      }
      return next;
    });

  const description =
    "Retained records, generated outputs, workflow revisions, and recoverable deletions of this project. Nothing is removed automatically; referenced or shared items are kept unless you include them.";

  if (!root) {
    return (
      <SettingsPage title="Storage & History" description={description} testId="settings-storage">
        <EmptyState title="No folder open" description="Open a project folder to see what it retains." />
      </SettingsPage>
    );
  }

  return (
    <SettingsPage
      title="Storage & History"
      description={description}
      testId="settings-storage"
      actions={
        <Button size="sm" variant="ghost" icon={RefreshCw} isPending={loading} onPress={() => void load()}>
          Refresh
        </Button>
      }
    >
      {overview === null ? (
        error ? (
          <ErrorState title="Storage overview is unavailable" details={error} actions={<Button onPress={() => void load()}>Retry</Button>} />
        ) : (
          <LoadingState title="Measuring retained data…" />
        )
      ) : (
        <>
          <div className="exp-storage__scope" data-testid="storage-scope" data-count={scope.items.length}>
            <div className="exp-storage__scope-text" aria-live="polite">
              {scope.items.length === 0 ? (
                <span className="ui-meta">Select items to purge.</span>
              ) : (
                <>
                  <strong className="exp-storage__scope-summary">
                    {scope.items.length} selected · {formatBytes(scope.bytes)} of files
                    {scope.unmeasured ? ` (+${scope.unmeasured} not measured)` : ""}
                  </strong>
                  <span className="ui-meta">
                    Purge permanently deletes the selected items. Removing records frees database space only when the database is compacted;
                    the result says what was actually reclaimed.
                    {scope.referenced ? ` ${scope.referenced} referenced item${scope.referenced === 1 ? " is" : "s are"} included: you will see what depends on ${scope.referenced === 1 ? "it" : "them"} first.` : ""}
                    {scope.active ? ` ${scope.active} active item${scope.active === 1 ? " is" : "s are"} included: work is stopped only if you choose to.` : ""}
                  </span>
                </>
              )}
            </div>
            <Button variant="danger" icon={Trash2} isDisabled={scope.items.length === 0} isPending={purging} onPress={() => void purge()} data-testid="storage-purge">
              {scope.items.length ? `Purge ${scope.items.length} Selected` : "Purge Selected"}
            </Button>
          </div>
          {overview.categories.length === 0 && <EmptyState title="Nothing retained" description="Runs, task records, outputs, and deletions appear here." />}
          {overview.categories.map((category) => (
            <CategoryGroup key={category.id} category={category} selected={selected} onToggle={toggle} onSelectAll={selectMany} />
          ))}
          {overview.excluded.length > 0 && (
            <Banner tone="info" title="Not included here" className="exp-storage__excluded">
              <ul data-testid="storage-excluded">
                {overview.excluded.map((e) => (
                  <li key={e}>{e}</li>
                ))}
              </ul>
            </Banner>
          )}
        </>
      )}
    </SettingsPage>
  );
}
