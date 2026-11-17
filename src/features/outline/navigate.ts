// Outline navigation: select an item's source range in the active editor and
// show the same place on the pages when the page view can map it (the
// Problems panel's reveal path). Read-only: only the caret/selection and the
// scroll position change.
//
// An outline answers one document revision. When the text changed since,
// the outline is recomputed and the item re-located only when it can be
// identified with certainty (`relocate`); a range that no longer exists is
// never used to jump to an unrelated declaration.
import { editors, useEditors } from "../../app/editors";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import type { DocumentOutline } from "../../bindings/DocumentOutline";
import type { OutlineItem } from "../../bindings/OutlineItem";
import { handleForPath, type DocumentHandle } from "../editor";
import { activeSourceEditor } from "../editor/find";
import { revealSource } from "../editor/reveal";
import { requestPage, requestReveal } from "../typesetting/store";
import { headerOf, itemName, relocate } from "./model";
import type { OutlineStore } from "./store";

/** The outline answers exactly the text the editor shows now. */
function answersCurrentText(handle: DocumentHandle, outline: DocumentOutline): boolean {
  return (
    !handle.closed &&
    !handle.model.isDisposed() &&
    handle.sync.revision === outline.revision &&
    handle.sync.pendingCount === 0
  );
}

/** The item's first line is where the outline says it is. */
function headerMatches(handle: DocumentHandle, item: OutlineItem): boolean {
  const model = handle.model;
  if (item.selection.end > model.getValueLength()) return false;
  const position = model.getPositionAt(item.selection.start);
  const line = model.getLineContent(position.lineNumber).slice(position.column - 1);
  return headerOf(line) === item.header;
}

function refuse(item: OutlineItem): "refused" {
  notify({
    tone: "info",
    title: `${itemName(item)} is no longer at this place in the source`,
    body: "The outline was refreshed.",
  });
  return "refused";
}

export async function navigateToItem(
  item: OutlineItem,
  shown: DocumentOutline,
  store: OutlineStore,
): Promise<"shown" | "refused"> {
  const handle = handleForPath(shown.path);
  if (!handle) return refuse(item);
  let target = item;
  if (!answersCurrentText(handle, shown)) {
    await handle.sync.idle();
    const fresh = await store.refresh();
    const moved = fresh ? relocate(item, shown, fresh) : null;
    if (!fresh || !moved || !answersCurrentText(handle, fresh)) return refuse(item);
    target = moved;
  }
  if (!headerMatches(handle, target)) {
    void store.refresh();
    return refuse(item);
  }
  await showRange(shown.path, target);
  return "shown";
}

async function showRange(path: string, item: OutlineItem): Promise<void> {
  const active = editors.active();
  const modes = active ? (registry.editorFor(active.resource)?.modes ?? []) : [];
  if (active?.resource.path === path && modes.includes("page")) {
    // Typeset documents: the shared reveal selects the range in the source
    // and shows it on the pages when the page view can map it (else the
    // page-only view opens the source beside the pages).
    requestReveal(path, item.selection.start, item.selection.end);
    if (active.mode === "split") setTimeout(() => activeSourceEditor()?.focus(), 0);
    return;
  }
  await revealSource(path, item.selection);
}

/** Show page `index` of the active typeset document (the source view gains the pages beside it). */
export async function showPage(path: string, index: number): Promise<void> {
  const active = editors.active();
  if (!active || active.resource.path !== path) return;
  if (active.mode === "source") editors.setMode(active.resource.key, "split");
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const group = useEditors.getState().activeGroup;
    const view = document.querySelector(
      `.wb-editor-group[data-group="${group}"] [role="tabpanel"]:not([inert]) [data-testid="page-editor"]`,
    );
    if (view) break;
    await new Promise((r) => setTimeout(r, 50));
  }
  requestPage(path, index);
}
