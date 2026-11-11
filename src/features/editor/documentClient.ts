// Document client: one Monaco model per open document, kept in sync with the
// document service (the only authority) through `DocumentSync`.
//
// - Opens project files (`document_open`) and attaches by id (`document_state`).
// - Applies service events from other origins to the model without echo and
//   without entering Monaco's own undo stack (`model.applyEdits`).
// - Routes undo/redo to the shared service history.
// - Tracks dirty state for tabs, disk conflicts, save errors, and close guards.
// - Closes the service document when its editor tab closes.
import { editors, useEditors } from "../../app/editors";
import { dismiss, notify } from "../../app/notifications";
import type { DiskConflict } from "../../bindings/DiskConflict";
import type { DocumentChanged } from "../../bindings/DocumentChanged";
import type { DocumentEvent } from "../../bindings/DocumentEvent";
import type { DocumentId } from "../../bindings/DocumentId";
import type { DocumentInfo } from "../../bindings/DocumentInfo";
import type { DocumentState } from "../../bindings/DocumentState";
import type { LineEnding } from "../../bindings/LineEnding";
import type { TextPatch } from "../../bindings/TextPatch";
import type { TextRange } from "../../bindings/TextRange";
import { commands, errorMessage, events, isCommandError } from "../../lib/ipc";
import { monaco } from "../../lib/monaco";
import { confirm, confirmUnsaved } from "../../ui/components";
import { classifyEol, textFitsEol, type EolMode } from "./eol";
import { ensureLanguage } from "./languages";
import { diffPatch } from "./ot";
import { useAppSettings } from "./settings";
import { DocumentSync } from "./sync";
import { UndoGrouper } from "./typing";

/** Editor resource key for a workspace file. */
export const fileKey = (path: string) => `file:${path}`;

const baseName = (path: string | null) => (path ? (path.split("/").pop() ?? path) : "Untitled");

type Listener = () => void;

export class DocumentHandle {
  readonly id: DocumentId;
  readonly model: monaco.editor.ITextModel;
  readonly sync: DocumentSync;
  readonly grouper = new UndoGrouper();
  info: DocumentInfo;
  conflict: DiskConflict | null = null;
  saveError: string | null = null;
  eolMode: EolMode;
  /** Bumped on every observable change (for `useSyncExternalStore`). */
  version = 0;
  /** Selection before the next local edit, for undo/redo restoration. */
  selection: TextRange[] = [];
  private applying = false;
  private dirty = false;
  private listeners = new Set<Listener>();
  private disposables: monaco.IDisposable[] = [];
  closed = false;

  constructor(state: DocumentState) {
    this.id = state.info.id;
    this.info = state.info;
    this.eolMode = classifyEol(state.text);
    const language = ensureLanguage(state.info.language);
    const uri = monaco.Uri.from({ scheme: "exposition", path: `/${state.info.id}/${state.info.path ?? "untitled"}` });
    this.model = monaco.editor.createModel(state.text, language, uri);
    this.fixEol();
    this.sync = new DocumentSync(
      this.id,
      state.info.revision,
      {
        apply: (request) => commands.documentApply(request),
        undo: (document) => commands.documentUndo(document, { kind: "source" }),
        redo: (document) => commands.documentRedo(document, { kind: "source" }),
        state: (document) => commands.documentState(document),
      },
      {
        applyPatches: (patches) => this.applyToModel(patches),
        canApply: (patches) => this.canApply(patches),
        setText: (text) => this.setModelText(text),
        setSelection: (ranges) => this.emitSelection(ranges),
      },
      {
        onChange: () => this.changed(),
        onRejected: (error) => {
          notify({
            tone: "warning",
            title: `An edit to ${this.name} was not applied`,
            body: `${errorMessage(error)}. The editor was refreshed from the document.`,
          });
        },
      },
    );
    this.disposables.push(this.model.onDidChangeContent((e) => this.onLocalChange(e)));
  }

  get path(): string | null {
    return this.info.path;
  }

  get name(): string {
    return baseName(this.info.path);
  }

  get key(): string | null {
    return this.info.path ? fileKey(this.info.path) : null;
  }

  get isDirty(): boolean {
    return this.info.savedRevision !== this.sync.revision || this.sync.pendingCount > 0;
  }

  /** Why the source editor cannot be edited right now, if it cannot. */
  get readOnlyReason(): string | null {
    if (this.info.readOnly) return this.info.readOnly;
    if (this.eolMode === "mixed") return "This file mixes line endings. Normalize them to edit it here.";
    if (this.sync.isResyncing) return "Refreshing…";
    return null;
  }

  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getVersion = (): number => this.version;

  private selectionListeners = new Set<(ranges: TextRange[]) => void>();

  onSelection(listener: (ranges: TextRange[]) => void): () => void {
    this.selectionListeners.add(listener);
    return () => this.selectionListeners.delete(listener);
  }

  private emitSelection(ranges: TextRange[]) {
    for (const l of this.selectionListeners) l(ranges);
  }

  changed() {
    this.version += 1;
    const dirty = this.isDirty;
    if (dirty !== this.dirty && this.key) {
      this.dirty = dirty;
      editors.setDirty(this.key, dirty);
    }
    for (const l of this.listeners) l();
  }

  setInfo(info: DocumentInfo) {
    this.info = info;
    this.changed();
  }

  setConflict(conflict: DiskConflict | null) {
    this.conflict = conflict;
    this.changed();
  }

  setSaveError(message: string | null) {
    this.saveError = message;
    if (message === null) dismiss(`save-${this.id}`);
    this.changed();
  }

  // ----- Monaco model adapter ---------------------------------------------

  private defaultEol(): monaco.editor.EndOfLineSequence {
    return useAppSettings.getState().settings.editor.defaultEol === "crlf"
      ? monaco.editor.EndOfLineSequence.CRLF
      : monaco.editor.EndOfLineSequence.LF;
  }

  /** Give terminator-free text the configured EOL for newly typed lines. */
  private fixEol() {
    if (this.eolMode === "none") {
      this.applying = true;
      try {
        this.model.setEOL(this.defaultEol());
      } finally {
        this.applying = false;
      }
    }
  }

  private onLocalChange(e: monaco.editor.IModelContentChangedEvent) {
    if (this.applying || e.isFlush || this.closed) return;
    if (this.eolMode === "mixed") {
      // The view is a normalized display of mixed line endings; its offsets do
      // not match the document, so never send them. Restore the display.
      void this.sync.resync();
      return;
    }
    const patches: TextPatch[] = [...e.changes]
      .sort((a, b) => a.rangeOffset - b.rangeOffset)
      .map((c) => ({ range: { start: c.rangeOffset, end: c.rangeOffset + c.rangeLength }, text: c.text }));
    if (this.eolMode === "none" && patches.some((p) => /[\r\n]/.test(p.text))) {
      this.eolMode = this.model.getEOL() === "\r\n" ? "crlf" : "lf";
    }
    const undo = this.grouper.classify(patches, Date.now());
    let shift = 0;
    const after = patches.map((p) => {
      const at = p.range.start + shift + p.text.length;
      shift += p.text.length - (p.range.end - p.range.start);
      return { start: at, end: at };
    });
    const before = this.selection.length ? this.selection : patches.map((p) => p.range);
    this.selection = after;
    this.sync.local(patches, undo, { before, after });
  }

  private canApply(patches: TextPatch[]): boolean {
    if (this.eolMode === "mixed") return false;
    const eol = this.model.getEOL() === "\r\n" ? "\r\n" : "\n";
    const length = this.model.getValueLength();
    for (const p of patches) {
      for (const offset of [p.range.start, p.range.end]) {
        if (offset > length || this.model.getOffsetAt(this.model.getPositionAt(offset)) !== offset) return false;
      }
      if (!textFitsEol(p.text, eol)) return false;
    }
    return true;
  }

  private applyToModel(patches: TextPatch[]) {
    const edits = patches.map((p) => ({
      range: monaco.Range.fromPositions(this.model.getPositionAt(p.range.start), this.model.getPositionAt(p.range.end)),
      text: p.text,
    }));
    this.applying = true;
    try {
      this.model.applyEdits(edits);
    } finally {
      this.applying = false;
    }
    if (this.eolMode === "none" && patches.some((p) => /[\r\n]/.test(p.text))) {
      this.eolMode = this.model.getEOL() === "\r\n" ? "crlf" : "lf";
    }
    this.grouper.reset();
  }

  private setModelText(text: string) {
    const mode = classifyEol(text);
    this.applying = true;
    try {
      const modelEol = this.model.getEOL() === "\r\n" ? "crlf" : "lf";
      const exact = mode !== "mixed" && this.eolMode !== "mixed" && (mode === "none" || mode === modelEol);
      if (exact) {
        const patch = diffPatch(this.model.getValue(), text);
        if (patch) this.applyToModel([patch]);
      } else {
        this.model.setValue(text);
      }
    } finally {
      this.applying = false;
    }
    this.eolMode = mode;
    this.fixEol();
    if (mode !== "mixed" && this.model.getValue() !== text) {
      // Never edit a view that does not mirror the document exactly.
      this.eolMode = "mixed";
    }
    this.grouper.reset();
  }

  undo() {
    this.grouper.reset();
    this.sync.history("undo");
  }

  redo() {
    this.grouper.reset();
    this.sync.history("redo");
  }

  dispose() {
    this.closed = true;
    this.sync.dispose();
    for (const d of this.disposables) d.dispose();
    this.model.dispose();
    this.listeners.clear();
    this.selectionListeners.clear();
  }
}

// ----- registry of open handles --------------------------------------------

const handles = new Map<DocumentId, DocumentHandle>();
const opening = new Map<string, Promise<DocumentHandle>>();
const guardDisposers = new Map<DocumentId, () => void>();
/** Keys whose tabs have been seen open (a handle closes when its tab disappears). */
const seenKeys = new Set<string>();
let started = false;

/**
 * `DocumentEvent::Changed` is serialized with two `kind` keys (the enum tag
 * "changed" and `DocumentChanged.kind`); JSON parsing keeps the last one, so a
 * change event arrives with `kind` = "edit" | "undo" | "redo". Recognize it by
 * its revision fields instead of the tag.
 */
export function asChanged(event: DocumentEvent): DocumentChanged | null {
  return event.kind === "changed" ? event : null;
}

function start() {
  if (started) return;
  started = true;
  void events.documents((event) => {
    const changed = asChanged(event);
    if (changed) {
      handles.get(changed.document)?.sync.receive(changed);
      return;
    }
    switch (event.kind) {
      case "opened":
      case "infoChanged":
        handles.get(event.info.id)?.setInfo(event.info);
        break;
      case "closed": {
        const handle = handles.get(event.document);
        if (handle) disposeHandle(handle);
        break;
      }
    }
  });
  void events.documentsResync(() => {
    for (const handle of handles.values()) void handle.sync.resync();
  });
  void events.workspace((event) => {
    switch (event.kind) {
      case "conflict": {
        const handle = handles.get(event.conflict.document);
        handle?.setConflict(event.conflict);
        if (handle) {
          notify({
            id: `conflict-${handle.id}`,
            tone: "warning",
            title: event.conflict.kind === "deleted" ? `${handle.name} was deleted on disk` : `${handle.name} changed on disk`,
            body: event.conflict.message,
            timeoutMs: null,
          });
        }
        break;
      }
      case "conflictResolved":
        handles.get(event.document)?.setConflict(null);
        dismiss(`conflict-${event.document}`);
        break;
      case "documentReloaded": {
        const handle = handles.get(event.document);
        if (handle) notify({ tone: "info", title: `${handle.name} was reloaded`, body: "The file changed on disk." });
        break;
      }
      case "journalFailed":
        notify({ tone: "danger", title: `Recovery backup failed for ${event.path}`, body: event.message });
        break;
      case "closed":
        disposeAllHandles();
        break;
      default:
        break;
    }
  });
  // Close the service document when its editor tab closes.
  useEditors.subscribe((state) => {
    const open = new Set<string>();
    for (const group of Object.values(state.groups)) for (const e of group.editors) open.add(e.resource.key);
    for (const key of open) seenKeys.add(key);
    for (const handle of [...handles.values()]) {
      const key = handle.key;
      if (key && seenKeys.has(key) && !open.has(key)) {
        seenKeys.delete(key);
        void closeDocument(handle);
      }
    }
  });
}

function register(state: DocumentState, conflict: DiskConflict | null): DocumentHandle {
  const existing = handles.get(state.info.id);
  if (existing) return existing;
  const handle = new DocumentHandle(state);
  handle.conflict = conflict;
  handles.set(handle.id, handle);
  const key = handle.key;
  if (key) {
    guardDisposers.set(handle.id, editors.registerCloseGuard(key, () => closeGuard(handle)));
    if (editors.all().some((e) => e.resource.key === key)) seenKeys.add(key);
  }
  handle.changed();
  return handle;
}

function disposeHandle(handle: DocumentHandle) {
  if (handles.get(handle.id) !== handle) return;
  handles.delete(handle.id);
  guardDisposers.get(handle.id)?.();
  guardDisposers.delete(handle.id);
  // The document was closed in the service (e.g. its folder was closed): close
  // any tab still showing it (its guard is gone, so this does not prompt).
  const key = handle.key;
  if (key) {
    seenKeys.delete(key);
    if (editors.all().some((e) => e.resource.key === key)) void editors.close(key);
  }
  handle.dispose();
}

/** Forget every workspace-file handle (the service closed the folder's documents). */
export function disposeAllHandles() {
  for (const handle of [...handles.values()]) if (handle.path !== null) disposeHandle(handle);
}

/** Open a workspace file (or return its open handle). */
export function openDocument(path: string): Promise<DocumentHandle> {
  start();
  const existing = [...handles.values()].find((h) => h.path === path);
  if (existing) return Promise.resolve(existing);
  let pending = opening.get(path);
  if (!pending) {
    pending = commands
      .documentOpen(path)
      .then((opened) => register(opened.state, opened.conflict))
      .finally(() => opening.delete(path));
    opening.set(path, pending);
  }
  return pending;
}

/** Attach to a document that is already open in the service. */
export async function attachDocument(id: DocumentId): Promise<DocumentHandle> {
  start();
  const existing = handles.get(id);
  if (existing) return existing;
  const state = await commands.documentState(id);
  return register(state, null);
}

export function handleFor(id: DocumentId): DocumentHandle | undefined {
  return handles.get(id);
}

export function handleForPath(path: string): DocumentHandle | undefined {
  return [...handles.values()].find((h) => h.path === path);
}

export function openHandles(): DocumentHandle[] {
  return [...handles.values()];
}

async function closeDocument(handle: DocumentHandle) {
  disposeHandle(handle);
  try {
    await commands.documentClose(handle.id);
  } catch {
    // Already closed.
  }
}

async function closeGuard(handle: DocumentHandle): Promise<boolean> {
  await handle.sync.idle();
  if (!handle.isDirty) return true;
  const choice = await confirmUnsaved(handle.name);
  if (choice === "save") return saveDocument(handle);
  return choice === "discard";
}

// ----- saving and disk conflicts -----------------------------------------------

/** Save a document. Resolves true when the file was written. */
export async function saveDocument(handle: DocumentHandle, options: { overwrite?: boolean } = {}): Promise<boolean> {
  await handle.sync.idle();
  try {
    const info = await commands.documentSave(handle.id, options.overwrite ?? false);
    handle.setInfo(info);
    handle.setSaveError(null);
    handle.grouper.reset();
    return true;
  } catch (error) {
    if (isCommandError(error) && error.code === "file.modifiedOnDisk" && !options.overwrite) {
      const choice = await confirm<"cancel" | "reload" | "overwrite">({
        title: `${handle.name} changed on disk`,
        message:
          "Another program changed this file after it was opened. Overwrite the file with your version, or reload the version on disk (your changes stay in undo history)?",
        actions: [
          { id: "cancel", label: "Cancel", variant: "secondary" },
          { id: "reload", label: "Reload from Disk", variant: "secondary" },
          { id: "overwrite", label: "Overwrite", variant: "danger" },
        ],
        cancelId: "cancel",
        defaultId: "cancel",
        tone: "warning",
      });
      if (choice === "overwrite") return saveDocument(handle, { overwrite: true });
      if (choice === "reload") await reloadDocument(handle);
      return false;
    }
    const message = errorMessage(error);
    handle.setSaveError(message);
    notify({ id: `save-${handle.id}`, tone: "danger", title: `Could not save ${handle.name}`, body: message, timeoutMs: null });
    return false;
  }
}

/** Save every document with unsaved changes (including ones without an open editor). */
export async function saveAll(): Promise<boolean> {
  await Promise.all([...handles.values()].map((h) => h.sync.idle()));
  const outcome = await commands.documentSaveAll();
  for (const info of outcome.saved) {
    const handle = handles.get(info.id);
    handle?.setInfo(info);
    handle?.setSaveError(null);
  }
  for (const failure of outcome.failed) {
    const handle = handles.get(failure.document);
    if (handle && failure.code === "file.modifiedOnDisk") {
      await saveDocument(handle);
      continue;
    }
    handle?.setSaveError(failure.message);
    notify({
      id: `save-${failure.document}`,
      tone: "danger",
      title: `Could not save ${baseName(failure.path)}`,
      body: failure.message,
      timeoutMs: null,
    });
  }
  return outcome.failed.length === 0;
}

export async function reloadDocument(handle: DocumentHandle): Promise<void> {
  await handle.sync.idle();
  try {
    handle.setInfo(await commands.documentReload(handle.id));
    handle.setConflict(null);
  } catch (error) {
    notify({ tone: "danger", title: `Could not reload ${handle.name}`, body: errorMessage(error) });
  }
}

export async function keepBuffer(handle: DocumentHandle): Promise<void> {
  try {
    await commands.documentKeepBuffer(handle.id);
    handle.setConflict(null);
  } catch (error) {
    notify({ tone: "danger", title: `Could not resolve ${handle.name}`, body: errorMessage(error) });
  }
}

export async function normalizeLineEndings(handle: DocumentHandle, ending: LineEnding): Promise<void> {
  await handle.sync.idle();
  try {
    await commands.documentNormalizeLineEndings(handle.id, ending);
    // The resulting revision arrives as an event; the view resyncs and becomes editable.
  } catch (error) {
    notify({ tone: "danger", title: "Could not normalize line endings", body: errorMessage(error) });
  }
}

/** The document shown in the active editor tab, if it is a workspace file with an open handle. */
export function activeHandle(): DocumentHandle | undefined {
  const active = editors.active();
  return active?.resource.path ? handleForPath(active.resource.path) : undefined;
}
