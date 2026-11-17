// Pure outline logic: stale-response rules, re-locating an item in a newer
// outline without jumping to an unrelated one, and the compiled-page list of
// the identified build. No IPC, no React.
import type { BuildResult } from "../../bindings/BuildResult";
import type { DocumentOutline } from "../../bindings/DocumentOutline";
import type { OutlineItem } from "../../bindings/OutlineItem";
import { summarize, type EntryBuildState } from "../typesetting/store";

/** What an outline answer is for: one document of one project. */
export interface OutlineTarget {
  documentId: string;
  path: string;
  /** Project root the document belongs to (null without a project). */
  root: string | null;
}

export function sameTarget(a: OutlineTarget | null, b: OutlineTarget | null): boolean {
  return !!a && !!b && a.documentId === b.documentId && a.path === b.path && a.root === b.root;
}

/**
 * Whether an outline answer may replace what is shown: it must answer the
 * newest request, for the target still shown (same document, path, and
 * project), and must not be older than the outline already shown.
 */
export function acceptAnswer(
  answer: DocumentOutline,
  request: { seq: number; target: OutlineTarget },
  latest: { seq: number; target: OutlineTarget | null; shown: DocumentOutline | null },
): boolean {
  if (request.seq !== latest.seq) return false;
  if (!sameTarget(request.target, latest.target)) return false;
  if (answer.path !== request.target.path) return false;
  if (latest.shown && latest.shown.path === answer.path && answer.revision < latest.shown.revision) return false;
  return true;
}

/** Every item, depth first (source order). */
export function flatten(items: readonly OutlineItem[]): OutlineItem[] {
  const out: OutlineItem[] = [];
  const walk = (list: readonly OutlineItem[]) => {
    for (const item of list) {
      out.push(item);
      walk(item.children);
    }
  };
  walk(items);
  return out;
}

/** `construct|name` of an item id (`construct|name|occurrence`). */
function idBase(id: string): string {
  const cut = id.lastIndexOf("|");
  return cut < 0 ? id : id.slice(0, cut);
}

/**
 * The item `item` (from `before`) in the newer outline `after`, or null when
 * it cannot be identified with certainty: it must have the same key and the
 * same first line, and the number of items with its construct and name must
 * be unchanged (otherwise occurrence numbers may have shifted, and a removed
 * theorem would be confused with the next one written the same way).
 */
export function relocate(item: OutlineItem, before: DocumentOutline, after: DocumentOutline): OutlineItem | null {
  if (before.path !== after.path) return null;
  const base = idBase(item.id);
  const was = flatten(before.items).filter((i) => idBase(i.id) === base).length;
  const now = flatten(after.items).filter((i) => idBase(i.id) === base);
  if (was !== now.length) return null;
  const match = now.find((i) => i.id === item.id);
  return match && match.header === item.header ? match : null;
}

/** Whitespace-normalized first line, at most 200 characters (as the backend computes `header`). */
export function headerOf(line: string): string {
  return [...line.trimEnd().split(/\s+/).filter(Boolean).join(" ")].slice(0, 200).join("");
}

/** A row's visible name: "Theorem (Main result)", "Introduction", "Proof". */
export function itemName(item: OutlineItem): string {
  if (item.kind === "heading") return item.title || `${item.typeLabel} (untitled)`;
  return item.title ? `${item.typeLabel} (${item.title})` : item.typeLabel;
}

// ----- compiled pages ------------------------------------------------------------

export type PageListState =
  /** Pages of a successful build of the current revision. */
  | "current"
  /** Pages of a successful build; the source changed since (a newer build may follow). */
  | "earlier"
  /** The latest build failed: pages of the last successful build (an earlier revision). */
  | "failed"
  /** No successful build is known. */
  | "none";

export interface PageList {
  state: PageListState;
  /** Build the pages come from (an identified successful build), if any. */
  build: BuildResult | null;
  /** Document revision that build compiled. */
  buildRevision: number | null;
  pages: { index: number }[];
  building: boolean;
}

/** The compiled pages to list for a document, from the build state of its entry. */
export function pageList(entry: EntryBuildState | undefined, documentId: string | null, revision: number | null): PageList {
  const latest = entry?.latest ?? null;
  const succeeded = latest?.result.status.kind === "succeeded";
  const build = succeeded ? latest!.result : (latest?.lastGood ?? null);
  const building = summarize(entry) === "building";
  if (!build || build.status.kind !== "succeeded") return { state: "none", build: null, buildRevision: null, pages: [], building };
  const buildRevision = build.sources.find((s) => documentId !== null && s.document === documentId)?.revision ?? null;
  const pages = build.pages.map((p) => ({ index: p.index }));
  const state: PageListState = !succeeded
    ? "failed"
    : buildRevision !== null && revision !== null && buildRevision < revision
      ? "earlier"
      : "current";
  return { state, build, buildRevision, pages, building };
}
