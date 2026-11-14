// Settings › Storage & History (P1 spec §4): retained run/task records,
// generated outputs, workflow revisions, and recoverable app-managed
// deletions of the open project, with sizes where measurable. Purge removes
// the selected records/payloads through the shared operation client (one
// decision when the plan names a consequence). Referenced or shared items
// are kept by default: they are never part of "Select all", and including one
// leads to its impact decision. Nothing is removed automatically, and record
// removal is not reported as reclaimed disk space.
import { ArchiveRestore, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";

import type { StorageCategory } from "../../bindings/StorageCategory";
import type { StorageItem } from "../../bindings/StorageItem";
import type { StorageOverview } from "../../bindings/StorageOverview";
import { errorMessage } from "../../lib/ipc";
import { Badge, Banner, Button, Checkbox, EmptyState, ErrorState, LoadingState } from "../../ui/components";
import { onProjectOp, restoreAndReport, runUserOperation } from "../operations/client";
import { opCommands } from "../operations/ipc";
import { formatBytes, targetKey } from "../operations/model";
import { useWorkspace } from "../workspace/store";
import { SettingsGroup, SettingsPage } from "./parts";

export interface StorageScope {
  items: StorageItem[];
  /** Sum of measurable sizes. */
  bytes: number;
  /** Selected items whose size is not measurable. */
  unmeasured: number;
  referenced: number;
  active: number;
}

/** The selected scope: what Purge would act on, with honest size accounting. */
export function storageScope(overview: StorageOverview | null, selected: ReadonlySet<string>): StorageScope {
  const items = (overview?.categories ?? []).flatMap((c) => c.items).filter((i) => selected.has(targetKey(i.target)));
  return {
    items,
    bytes: items.reduce((sum, i) => sum + (i.bytes ?? 0), 0),
    unmeasured: items.filter((i) => i.bytes === null).length,
    referenced: items.filter((i) => i.referenced).length,
    active: items.filter((i) => i.active).length,
  };
}

/** Keys "Select all" adds for a category: never referenced/shared items. */
export function selectableByDefault(category: StorageCategory): string[] {
  return category.items.filter((i) => !i.referenced).map((i) => targetKey(i.target));
}

function formatDate(ms: number | null): string {
  return ms === null ? "—" : new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
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
  const title = `${category.label} (${category.items.length}${category.bytes !== null ? ` · ${formatBytes(category.bytes)}` : ""})`;
  return (
    <SettingsGroup
      title={title}
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
        <p className="exp-settings__group-description">Nothing retained.</p>
      ) : (
        <table className="exp-settings__table" aria-label={category.label}>
          <thead>
            <tr>
              <th scope="col" className="exp-settings__col-fit">
                <span className="ui-visually-hidden">Include</span>
              </th>
              <th scope="col">Item</th>
              <th scope="col" className="exp-settings__col-fit">
                Size
              </th>
              <th scope="col" className="exp-settings__col-fit">
                Created
              </th>
              <th scope="col" className="exp-settings__col-fit">
                State
              </th>
              {recoverable && (
                <th scope="col" className="exp-settings__col-fit">
                  <span className="ui-visually-hidden">Actions</span>
                </th>
              )}
            </tr>
          </thead>
          <tbody>
            {category.items.map((item) => {
              const key = targetKey(item.target);
              return (
                <tr key={key} data-testid="storage-item" data-key={key} data-referenced={item.referenced || undefined} data-active={item.active || undefined}>
                  <td className="exp-settings__col-fit">
                    <Checkbox
                      aria-label={`Include ${item.label}${item.referenced ? " (referenced; you will be asked first)" : ""}`}
                      isSelected={selected.has(key)}
                      onChange={(on) => onToggle(key, on)}
                    />
                  </td>
                  <th scope="row">{item.label}</th>
                  <td className="exp-settings__col-fit">{formatBytes(item.bytes)}</td>
                  <td className="exp-settings__col-fit">{formatDate(item.createdAtMs)}</td>
                  <td className="exp-settings__col-fit">
                    {item.referenced && (
                      <Badge tone="info" title="Kept by default; including it asks before anything is removed">
                        Referenced
                      </Badge>
                    )}
                    {item.active && <Badge tone="warning">Active</Badge>}
                    {!item.referenced && !item.active && <span className="xp-ops-hint">—</span>}
                  </td>
                  {recoverable && (
                    <td className="exp-settings__actions">
                      {item.target.kind === "recoverable" && (
                        <Button
                          size="sm"
                          variant="secondary"
                          icon={ArchiveRestore}
                          data-testid="storage-restore"
                          onPress={() =>
                            void restoreAndReport([{ token: (item.target as { id: string }).id, target: item.target }], new Map([[key, item.label]]))
                          }
                        >
                          Restore
                        </Button>
                      )}
                    </td>
                  )}
                </tr>
              );
            })}
          </tbody>
        </table>
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
      const keys = new Set(next.categories.flatMap((c) => c.items.map((i) => targetKey(i.target))));
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
  const labels = useMemo(() => new Map(scope.items.map((i) => [targetKey(i.target), i.label])), [scope]);

  const purge = async () => {
    setPurging(true);
    try {
      const done = await runUserOperation({ kind: "delete", permanent: true }, scope.items.map((i) => i.target), { labels });
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
                <span className="xp-ops-hint">Select items to purge. Sizes are shown where they can be measured.</span>
              ) : (
                <>
                  <strong>
                    {scope.items.length} selected · {formatBytes(scope.bytes)} of files
                    {scope.unmeasured ? ` (+${scope.unmeasured} not measured)` : ""}
                  </strong>
                  <span className="xp-ops-hint">
                    Purge removes the selected records and payloads permanently. Removing records frees database space only when the database is
                    compacted; the result says what was actually reclaimed.
                    {scope.referenced ? ` ${scope.referenced} referenced item${scope.referenced === 1 ? " is" : "s are"} included: you will see what depends on ${scope.referenced === 1 ? "it" : "them"} first.` : ""}
                    {scope.active ? ` ${scope.active} active item${scope.active === 1 ? " is" : "s are"} included: work is stopped only if you choose so.` : ""}
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
              <ul className="exp-storage__excluded-list" data-testid="storage-excluded">
                {overview.excluded.map((e) => (
                  <li key={e}>{e}</li>
                ))}
              </ul>
              These retained copies and backups are kept by their own services and are not purged from this page.
            </Banner>
          )}
        </>
      )}
    </SettingsPage>
  );
}
