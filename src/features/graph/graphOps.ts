// Graph operations (P1 spec §2 "Project graph and mathematical entities").
//
// Labels say exactly what happens:
// - Hide from view — presentation only (this saved view).
// - Delete relationship — a manual relationship (observed ones change with the source).
// - Delete entity — a manually created item (claim, question, collection, experiment…).
// - Delete source file/folder — a file-backed resource: its delete IS the file's
//   delete (the backend delegates to the file operation); never a metadata-only
//   delete that would reappear on reindex.
// Items that come from a source (theorems, sections, Lean declarations) are
// neither deleted nor duplicated here: that needs a source edit, so the actions
// are disabled with the reason, and Copy Reference / Copy Text are offered.
// Copies never carry verification status; the backend decides each copy contract.
import { ClipboardCopy, EyeOff, Quote } from "lucide-react";

import { notify } from "../../app/notifications";
import type { Entity } from "../../bindings/Entity";
import type { OpTarget } from "../../bindings/OpTarget";
import type { Relation } from "../../bindings/Relation";
import { commands, errorMessage } from "../../lib/ipc";
import { runUserOperation } from "../operations/client";
import { appClipboard, writeText } from "../operations/clipboard";
import { quoted, targetKey, trashName } from "../operations/model";
import type { ExtraItem } from "../operations/OpMenu";
import type { SurfaceAction, SurfaceActions } from "../operations/surfaces";
import { errorText, graphApi } from "./api";
import { entityDisplay, relationDisplay, type VocabularyIndex } from "./model";
import { revealSource } from "./reveal";

export type EntityClass = "project" | "managed" | "fileBacked" | "folder" | "sourceAnchored" | "manual";

/** How an entity may be deleted or duplicated from the graph. */
export function classifyEntity(entity: Entity, vocabulary: VocabularyIndex | null): EntityClass {
  if (entity.kind === "project") return "project";
  if (entity.kind === "researchSpace") return "managed";
  const display = entityDisplay(vocabulary, entity);
  if (display.kind.fileBacked) return entity.kind === "folder" ? "folder" : "fileBacked";
  if (entity.origin.kind === "extracted") return "sourceAnchored";
  return "manual";
}

/** A relationship the person (or an agent) asserted, which can be deleted; observed ones follow the source. */
export function relationDeletable(relation: Relation): boolean {
  return relation.origin.kind !== "extracted" && relation.status !== "observed";
}

const REASONS: Record<Exclude<EntityClass, "manual" | "fileBacked" | "folder">, { delete: string; duplicate: string }> = {
  project: { delete: "The project itself is not deleted from the graph", duplicate: "The project itself is not duplicated" },
  managed: { delete: "The research space is managed by Research", duplicate: "The research space is managed by Research" },
  sourceAnchored: {
    delete: "It comes from its source; change the source to remove it",
    duplicate: "It comes from its source and has no independent copy; use Copy Text or Copy Reference",
  },
};

export interface GraphSelection {
  entities: Entity[];
  relations: Relation[];
}

function plural(count: number, one: string, many: string) {
  return count === 1 ? one : many;
}

/** The delete label for a selection ("Delete Relationship", "Delete Source File", "Delete Entity", …). */
function deleteLabel(classes: EntityClass[], relations: number): string {
  if (classes.length === 0) return plural(relations, "Delete Relationship", "Delete Relationships");
  if (relations === 0 && classes.every((c) => c === "fileBacked")) return plural(classes.length, "Delete Source File", "Delete Source Files");
  if (relations === 0 && classes.every((c) => c === "folder")) return plural(classes.length, "Delete Source Folder", "Delete Source Folders");
  if (relations === 0 && classes.every((c) => c === "manual")) return plural(classes.length, "Delete Entity", "Delete Entities");
  return "Delete Selected Items";
}

async function copyText(entity: Entity): Promise<void> {
  try {
    const detail = await graphApi().entity(entity.id);
    const binding = detail.bindings.find((b) => b.state === "resolved" && b.range) ?? null;
    if (!binding?.range) {
      notify({ tone: "info", title: `No source text for ${quoted(entity.label)}`, body: "It is not bound to a range of a source file." });
      return;
    }
    // Read only a document that is already open (never open files, start servers, or change anything to copy).
    const open = (await commands.documentList()).find((d) => d.path === binding.path);
    if (!open) {
      notify({
        tone: "info",
        title: "Open the source to copy its text",
        body: `${quoted(entity.label)} is in ${binding.path}.`,
        actions: [{ label: "Open Source", run: () => void revealSource(binding.path, binding.range) }],
      });
      return;
    }
    const state = await commands.documentState(open.id);
    const text = state.text.slice(binding.range.start, binding.range.end);
    if (await writeText(text)) notify({ tone: "info", title: `Copied the text of ${quoted(entity.label)}`, body: `From ${binding.path}.` });
    else notify({ tone: "warning", title: "Could not copy to the clipboard" });
  } catch (error) {
    notify({ tone: "danger", title: "Could not copy the text", body: errorMessage(error) || errorText(error) });
  }
}

async function copyReference(entity: Entity): Promise<void> {
  const reference = entity.key ?? entity.label;
  if (await writeText(reference)) notify({ tone: "info", title: "Copied reference", body: reference });
  else notify({ tone: "warning", title: "Could not copy to the clipboard", body: reference });
}

export interface GraphActionOptions {
  vocabulary: VocabularyIndex | null;
  canWrite: boolean;
  onHide?: (ids: string[]) => void;
  /** Select created copies (entities). */
  afterCreate?: (entities: string[]) => void;
}

/** Operation actions and extra menu items for a graph selection (canvas, table, inspector). */
export function graphActions(selection: GraphSelection, options: GraphActionOptions): { actions: SurfaceActions; extras: ExtraItem[] } {
  const { entities, relations } = selection;
  const classes = entities.map((e) => classifyEntity(e, options.vocabulary));
  const count = entities.length + relations.length;
  const labels = new Map<string, string>([
    ...entities.map((e) => [targetKey({ kind: "graphEntity", entity: e.id }), e.label] as const),
    ...relations.map((r) => [targetKey({ kind: "graphRelation", relation: r.id }), relationDisplay(options.vocabulary, r.kind).reading] as const),
  ]);
  const readOnly = options.canWrite ? null : "The graph is read-only right now";
  const none = "Select an item or relationship";

  const blockedDelete = (() => {
    for (const [i, c] of classes.entries()) {
      if (c === "project" || c === "managed" || c === "sourceAnchored") return `${quoted(entities[i]!.label)}: ${REASONS[c].delete}`;
    }
    const observed = relations.find((r) => !relationDeletable(r));
    if (observed) return "An observed relationship follows its source; change the source to remove it";
    return null;
  })();
  const blockedDuplicate = (() => {
    if (relations.length) return "Relationships are duplicated together with the items they connect";
    for (const [i, c] of classes.entries()) {
      if (c === "project" || c === "managed" || c === "sourceAnchored") return `${quoted(entities[i]!.label)}: ${REASONS[c].duplicate}`;
    }
    return null;
  })();

  const targets: OpTarget[] = [
    ...entities.map((e): OpTarget => ({ kind: "graphEntity", entity: e.id })),
    ...relations.map((r): OpTarget => ({ kind: "graphRelation", relation: r.id })),
  ];
  const deleteEnabled = count > 0 && !blockedDelete && !readOnly;
  const files = classes.filter((c) => c === "fileBacked" || c === "folder").length;
  const deleteDescription = files ? `Source ${plural(files, "file moves", "files move")} to ${trashName()}` : undefined;

  const createdEntities = (outcome: Awaited<ReturnType<typeof runUserOperation>>) =>
    (outcome?.result.outcomes ?? []).flatMap((o) => (o.status === "succeeded" && o.created?.kind === "graphEntity" ? [o.created.entity] : []));

  const del = (permanent: boolean): SurfaceAction => ({
    enabled: deleteEnabled,
    reason: count === 0 ? none : (readOnly ?? blockedDelete ?? undefined),
    label: permanent ? "Delete Permanently" : deleteLabel(classes, relations.length),
    description: permanent ? "Cannot be restored" : deleteDescription,
    run: async () => {
      await runUserOperation({ kind: "delete", permanent }, targets, { labels });
    },
  });

  const clip = appClipboard.get();
  const actions: SurfaceActions = {
    copy: {
      enabled: entities.length > 0 && relations.length === 0,
      reason: entities.length === 0 ? "Select items to copy" : "Relationships are copied with the items they connect",
      description: "For Paste in the graph; names go to the clipboard as text",
      run: async () => {
        appClipboard.set({ kind: "graph", entities: entities.map((e) => ({ id: e.id, label: e.label })) });
        await writeText(entities.map((e) => e.key ?? e.label).join("\n"));
        notify({ tone: "info", title: entities.length === 1 ? `Copied ${quoted(entities[0]!.label)}` : `Copied ${entities.length} items` });
      },
    },
    paste: {
      enabled: clip?.kind === "graph" && clip.entities.length > 0 && !readOnly,
      reason: readOnly ?? "Copy graph items first",
      label: clip?.kind === "graph" && clip.entities.length > 0 ? `Paste ${plural(clip.entities.length, "Item", `${clip.entities.length} Items`)}` : "Paste",
      description: "New copies with their own identities",
      run: async () => {
        const current = appClipboard.get();
        if (current?.kind !== "graph") return;
        const pasteLabels = new Map(current.entities.map((e) => [targetKey({ kind: "graphEntity", entity: e.id }), e.label]));
        const outcome = await runUserOperation(
          { kind: "paste", destination: { kind: "project" } },
          current.entities.map((e) => ({ kind: "graphEntity", entity: e.id })),
          { labels: pasteLabels },
        );
        const created = createdEntities(outcome);
        if (created.length) options.afterCreate?.(created);
      },
    },
    duplicate: {
      enabled: entities.length > 0 && !blockedDuplicate && !readOnly,
      reason: count === 0 ? none : (readOnly ?? blockedDuplicate ?? undefined),
      description: files ? "Duplicates the source file with a new identity" : "A new item; verification is not copied",
      run: async () => {
        const created = createdEntities(await runUserOperation({ kind: "duplicate" }, targets, { labels }));
        if (created.length) options.afterCreate?.(created);
      },
    },
    delete: del(false),
    deletePermanently: del(true),
  };

  const extras: ExtraItem[] = [];
  if (options.onHide && entities.length > 0) {
    extras.push({
      id: "hide",
      label: "Hide from View",
      icon: EyeOff,
      description: "This view only; nothing is deleted",
      run: () => options.onHide?.(entities.map((e) => e.id)),
    });
  }
  if (entities.length === 1 && relations.length === 0) {
    const entity = entities[0]!;
    if (classes[0] === "sourceAnchored" || entity.key) {
      extras.push({ id: "copyReference", label: "Copy Reference", icon: Quote, description: entity.key ?? entity.label, run: () => copyReference(entity) });
    }
    if (classes[0] === "sourceAnchored") {
      extras.push({ id: "copyText", label: "Copy Text", icon: ClipboardCopy, description: "Its exact source text", run: () => copyText(entity) });
    }
  }
  return { actions, extras };
}
