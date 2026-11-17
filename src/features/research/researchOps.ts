// Operations in the Research explorers (P1): research files and retained
// results are project files (copied/duplicated/deleted through the file
// operations, with impact handling for evidence and manuscript use);
// experiments and external references are graph items; group folders are
// managed and saved workflows are changed in the Workflows tab.
import { FileOutput } from "lucide-react";

import { notify } from "../../app/notifications";
import type { OpTarget } from "../../bindings/OpTarget";
import type { ResearchListing } from "../../bindings/ResearchListing";
import type { RunSummary } from "../../bindings/RunSummary";
import { runUserOperation } from "../operations/client";
import { appClipboard } from "../operations/clipboard";
import { folderLabel, parentFolder, quoted, targetKey, trashName } from "../operations/model";
import type { ExtraItem } from "../operations/OpMenu";
import type { SurfaceActions } from "../operations/surfaces";
import { copySelection, createdPaths, pasteInto, type FileSelectionItem } from "../workspace/fileOps";
import { parseItemKey } from "./space";

export type ResearchSelectionItem =
  | { kind: "file"; path: string; name: string }
  | { kind: "group"; folder: string; name: string }
  | { kind: "experiment"; entity: string; name: string }
  | { kind: "reference"; entity: string; name: string }
  | { kind: "workflow"; entity: string; name: string };

/** Resolve explorer keys (`file:…`, `result:…`, `experiment:…`, `group:sources`, `rgroup:data`) to items. */
export function resolveResearchKeys(keys: Iterable<string>, listing: ResearchListing | null): ResearchSelectionItem[] {
  if (!listing?.space) return [];
  const root = listing.space.root;
  const files = new Map([...listing.sources, ...listing.data, ...listing.notes].map((f) => [f.path, f.name]));
  const items: ResearchSelectionItem[] = [];
  for (const key of keys) {
    const group = /^r?group:(sources|data|notes)$/.exec(key);
    if (group) {
      items.push({ kind: "group", folder: `${root}/${group[1]}`, name: group[1]! });
      continue;
    }
    const item = parseItemKey(key);
    if (!item) continue;
    if (item.kind === "file") {
      const name = files.get(item.path);
      if (name !== undefined) items.push({ kind: "file", path: item.path, name });
    }
    else if (item.kind === "result") {
      const result = listing.results.find((r) => r.entity === item.entity);
      if (result) items.push({ kind: "file", path: result.path, name: result.label });
    } else if (item.kind === "experiment" && item.entity) {
      const experiment = listing.experiments.find((e) => e.entity === item.entity);
      items.push({ kind: "experiment", entity: item.entity, name: experiment?.label ?? item.entity });
    } else if (item.kind === "reference") {
      const reference = listing.references.find((r) => r.entity === item.entity);
      items.push({ kind: "reference", entity: item.entity, name: reference?.label ?? item.entity });
    } else if (item.kind === "workflow") {
      const workflow = listing.workflows.find((w) => w.entity === item.entity);
      items.push({ kind: "workflow", entity: item.entity, name: workflow?.label ?? item.entity });
    }
  }
  return items;
}

function target(item: ResearchSelectionItem): OpTarget | null {
  if (item.kind === "file") return { kind: "path", path: item.path };
  if (item.kind === "experiment" || item.kind === "reference") return { kind: "graphEntity", entity: item.entity };
  return null;
}

const GROUP_TITLES: Record<string, string> = { sources: "Sources", data: "Data", notes: "Notes" };

/** "Data" for `<space root>/data` (the research space's managed group folders). */
function groupTitle(folder: string | null | undefined): string | null {
  const name = folder?.split("/").pop() ?? "";
  return folder && /^\.exposition\/research\/[^/]+\/(sources|data|notes)$/.test(folder) ? (GROUP_TITLES[name] ?? null) : null;
}

/** The operation actions for a Research explorer selection. */
export function researchActions(
  items: ResearchSelectionItem[],
  options: { root: string | null; afterCreate?: (paths: string[]) => void },
): SurfaceActions {
  const none = "Select a research file, result, or experiment";
  const files = items.filter((i): i is Extract<ResearchSelectionItem, { kind: "file" }> => i.kind === "file");
  const groups = items.filter((i) => i.kind === "group");
  const experiments = items.filter((i) => i.kind === "experiment");
  const workflows = items.filter((i) => i.kind === "workflow");
  const references = items.filter((i) => i.kind === "reference");
  const labels = new Map(items.flatMap((i) => {
    const t = target(i);
    return t ? [[targetKey(t), i.name] as const] : [];
  }));
  const fileItems: FileSelectionItem[] = files.map((f) => ({ path: f.path, kind: "file", name: f.name }));

  // Where a paste goes: a group folder, or beside one selected file.
  const pasteFolder =
    items.length === 1 && groups.length === 1
      ? (groups[0] as Extract<ResearchSelectionItem, { kind: "group" }>).folder
      : items.length === 1 && files.length === 1
        ? parentFolder(files[0]!.path)
        : undefined;

  const blocked = workflows.length
    ? "Saved workflows are changed in the Workflows tab"
    : groups.length
      ? "Sources, Data, and Notes are managed folders of the research space"
      : null;
  const deletable = items.length > 0 && !blocked;
  const targets = items.flatMap((i) => {
    const t = target(i);
    return t ? [t] : [];
  });
  const copyable = files.length > 0 && files.length === items.length;
  const duplicable = items.length > 0 && !blocked && references.length === 0;

  return {
    copy: {
      enabled: copyable || (experiments.length > 0 && experiments.length === items.length),
      reason: items.length === 0 ? none : "Copy files and experiments separately; references and groups are not copied",
      run: async () => {
        if (copyable) await copySelection(fileItems);
        else {
          appClipboard.set({ kind: "graph", entities: experiments.map((e) => ({ id: (e as { entity: string }).entity, label: e.name })) });
          notify({
            tone: "info",
            title: experiments.length === 1 ? `Copied experiment “${experiments[0]!.name}”` : `Copied ${experiments.length} experiments`,
            body: "Pasting on the Graph page makes a separate copy.",
          });
        }
      },
    },
    paste: {
      enabled: pasteFolder !== undefined,
      reason: "Select Sources, Data, Notes, or one file to paste beside it",
      label: pasteFolder === undefined ? "Paste" : `Paste into ${groupTitle(pasteFolder) ?? folderLabel(pasteFolder ?? null)}`,
      run: async () => {
        if (pasteFolder === undefined) return;
        const created = createdPaths(await pasteInto(pasteFolder, options.root));
        if (created.length) options.afterCreate?.(created);
      },
    },
    duplicate: {
      enabled: duplicable,
      reason: items.length === 0 ? none : blocked ?? "A read-only reference to an outside file is not duplicated",
      description: "Copies get new identities; nothing is attached or verified",
      run: async () => {
        const created = createdPaths(await runUserOperation({ kind: "duplicate" }, targets, { labels }));
        if (created.length) options.afterCreate?.(created);
      },
    },
    delete: {
      enabled: deletable,
      reason: items.length === 0 ? none : blocked ?? undefined,
      description: files.length ? `Files move to ${trashName()}` : undefined,
      run: async () => {
        await runUserOperation({ kind: "delete", permanent: false }, targets, { labels });
      },
    },
    deletePermanently: {
      enabled: deletable,
      reason: items.length === 0 ? none : blocked ?? undefined,
      description: "Cannot be restored",
      run: async () => {
        await runUserOperation({ kind: "delete", permanent: true }, targets, { labels });
      },
    },
  };
}

/** How a research run is named in decisions and notifications: its code's first line. */
export function researchRunLabel(run: Pick<RunSummary, "id" | "codePreview">): string {
  const line = run.codePreview.split(/\r?\n/).find((l) => l.trim() !== "")?.trim() ?? "";
  const short = line.length > 40 ? `${line.slice(0, 39)}…` : line;
  return short ? `run ${quoted(short)}` : `run ${run.id}`;
}

/**
 * Actions for selected research runs in a run history (XR-A-03), as for
 * workflow runs: Delete removes the record only (code, transcript;
 * restorable from Storage & History) and its outputs stay; "…and Its
 * Outputs" adds the outputs as their own target in the same request (held for
 * restore; files the project keeps stay). Active runs are coordinated through
 * the plan's choices. The backend decides impacts and recovery.
 */
export function researchRunActions(selected: RunSummary[]): { actions: SurfaceActions; extras: ExtraItem[] } {
  const has = selected.length > 0;
  const none = "Select a run first";
  const one = selected.length === 1;
  const targets: OpTarget[] = selected.map((r) => ({ kind: "researchRun", run: r.id }));
  const outputTargets: OpTarget[] = selected.map((r) => ({ kind: "runOutputs", run: r.id }));
  const labels = new Map<string, string>();
  for (const r of selected) {
    labels.set(targetKey({ kind: "researchRun", run: r.id }), researchRunLabel(r));
    labels.set(targetKey({ kind: "runOutputs", run: r.id }), `outputs of ${researchRunLabel(r)}`);
  }
  const active = selected.some((r) => r.status.kind === "running" || r.queued);
  const remove = (permanent: boolean, outputs: boolean) => async () => {
    await runUserOperation({ kind: "delete", permanent }, outputs ? [...targets, ...outputTargets] : targets, { labels });
  };
  return {
    actions: {
      copy: { enabled: false, reason: "A run is not copied; run its code again instead", run: () => {} },
      delete: {
        enabled: has,
        reason: none,
        label: one || !has ? "Delete Run Record" : "Delete Run Records",
        description: active ? "Running work is stopped first if you choose" : "The record only; outputs stay",
        run: remove(false, false),
      },
      deletePermanently: {
        enabled: has,
        reason: none,
        description: "The record cannot be restored; outputs stay",
        run: remove(true, false),
      },
    },
    extras: [
      {
        id: "deleteWithOutputs",
        label: one || !has ? "Delete Run and Its Outputs" : "Delete Runs and Their Outputs",
        icon: FileOutput,
        description: "Outputs are held for restore (their files stay on disk until purged); never inputs or files the project keeps",
        isDisabled: !has,
        run: remove(false, true),
      },
      {
        id: "deleteWithOutputsPermanently",
        label: one || !has ? "Delete Run and Its Outputs Permanently…" : "Delete Runs and Their Outputs Permanently…",
        icon: FileOutput,
        description: "Removes the record and the output files; cannot be restored",
        isDisabled: !has,
        run: remove(true, true),
      },
    ],
  };
}
