// Page editing session: one document shown as its compiled pages.
//
// The caret and selection live in source offsets of a specific document
// revision. Geometry comes from the page map of the displayed build; offsets
// are mapped between the caret's revision and the build's revision through
// the patches of every revision in between (DocumentChanged events). Edits are
// queued and sent one at a time as page transactions; the next build arrives
// as an event and the caret is re-placed from the new map. Typing never waits
// for a build.
import type { Affinity } from "../../bindings/Affinity";
import type { BuildEvent } from "../../bindings/BuildEvent";
import type { BuildResult } from "../../bindings/BuildResult";
import type { CaretGeometry } from "../../bindings/CaretGeometry";
import type { DocumentChanged } from "../../bindings/DocumentChanged";
import type { DocumentEvent } from "../../bindings/DocumentEvent";
import type { EditMode } from "../../bindings/EditMode";
import type { EditRefusal } from "../../bindings/EditRefusal";
import type { Motion } from "../../bindings/Motion";
import type { NonEditableReason } from "../../bindings/NonEditableReason";
import type { PageEditOp } from "../../bindings/PageEditOp";
import type { PageRect } from "../../bindings/PageRect";
import type { TrustChanged } from "../../bindings/TrustChanged";
import { commands, errorMessage, events, isCommandError, newId } from "../../lib/ipc";
import { openDocument, type DocumentHandle } from "../editor";
import { requestWorkspaceTrust } from "../jobs/trust";
import { buildSequence, isOlder } from "./buildOrder";
import { imageType } from "./images";
import { RevisionHistory } from "./mapping";

export interface PageImage {
  index: number;
  widthPt: number;
  heightPt: number;
  renderRef: string;
  url: string | null;
}

export interface PageMessage {
  tone: "info" | "warning";
  text: string;
}

/** A finished PDF export. */
export interface PdfExport {
  /** The file written. */
  path: string;
  /**
   * Why the exported build is not the current source: its latest build
   * `failed` (the last successful build was exported), or the source
   * `changed` after the exported build. `null` when it is current.
   */
  outdated: "failed" | "changed" | null;
}

export interface PageViewModel {
  phase: "attaching" | "ready" | "missing" | "error";
  error: string | null;
  documentId: string | null;
  pages: PageImage[];
  buildId: string | null;
  /** Document revision the displayed build was compiled from. */
  buildRevision: number | null;
  /** The pages shown do not reflect the current source yet (or it has errors). */
  stale: boolean;
  /** The latest build failed; the last good pages remain visible. */
  failed: boolean;
  errorCount: number;
  caret: CaretGeometry | null;
  selection: PageRect[];
  /** Rendered text of the selection (copied to the clipboard). */
  selectionText: string;
  hasSelection: boolean;
  composition: string;
  message: PageMessage | null;
  /** Edit-to-render latency of the latest edit (ms), for status display. */
  latencyMs: number | null;
  /** Builds of this document need workspace trust (LaTeX): the workspace root. */
  untrustedRoot: string | null;
  /**
   * Why the document cannot be edited in any view (the document service
   * refuses its edits), e.g. mixed line endings; null when it can.
   */
  readOnly: string | null;
}

interface Position {
  offset: number;
  revision: number;
}

const NON_EDITABLE: Record<NonEditableReason, string> = {
  generated: "Generated content (numbering, markers, caption labels) — edit in source",
  code: "Produced by code — edit in source",
  transformed: "Text transformed by a show rule or function — edit in source",
  raw: "Raw/verbatim text — edit in source",
  external: "Content from a package — not editable here",
};

function refusalText(refusal: EditRefusal): string {
  switch (refusal.kind) {
    case "notEditable":
      return `Not editable on the page (${refusal.reason}) — edit in source`;
    case "wouldChangeStructure":
      return "That edit would change the document structure here — edit in source";
    case "invalidPosition":
      return "Invalid position";
    case "noChange":
      return "";
  }
}


function isUnknownBuild(error: unknown): boolean {
  return isCommandError(error) && (error.code === "page.unknownBuild" || error.code === "page.noMap");
}

/** Stale retries per keystroke (each waits for a newer revision's event). */
const MAX_STALE_RETRIES = 20;
/** How long a stale keystroke waits for the concurrent change's event. */
const EVENT_WAIT_MS = 3000;
/** Pages loaded before a build is first shown, until the view reports what it displays. */
const FIRST_PAGES = 3;
/** Page images kept for pages that are not displayed (least recently displayed dropped first). */
export const IDLE_PAGE_IMAGES = 12;

export class PageSession {
  private model: PageViewModel = {
    phase: "attaching",
    error: null,
    documentId: null,
    pages: [],
    buildId: null,
    buildRevision: null,
    stale: true,
    failed: false,
    errorCount: 0,
    caret: null,
    selection: [],
    selectionText: "",
    hasSelection: false,
    composition: "",
    message: null,
    latencyMs: null,
    untrustedRoot: null,
    readOnly: null,
  };
  private listeners = new Set<() => void>();
  private docId: string | null = null;
  /** The shared document handle (also behind any source editor of this file). */
  private handle: DocumentHandle | null = null;
  private docRevision = 0;
  /** Highest revision whose change (history step) this view has received. */
  private eventRevision = 0;
  private revisionWaiters: Array<{ revision: number; resolve: (received: boolean) => void }> = [];
  /** Text typed on the page that could not be applied; typing stays paused
   *  (and keeps collecting) until the caret is placed again. */
  private unapplied: string | null = null;
  private build: BuildResult | null = null;
  private buildRevision = 0;
  private caret: (Position & { affinity: Affinity }) | null = null;
  private anchor: Position | null = null;
  private goalX: number | null = null;
  private group = 1;
  private readonly history = new RevisionHistory();
  private readonly own = new Set<string>();
  private queue: Promise<void> = Promise.resolve();
  /** Loaded page images by render reference, least recently displayed first. */
  private readonly urls = new Map<string, string>();
  /** Page image requests in flight, by render reference. */
  private readonly loading = new Map<string, Promise<string>>();
  /** Page indexes the view displays now (its viewport and the pages next to it), ascending. */
  private displayed: number[] = [];
  /** The view is hidden (its tab is not the displayed one): it neither watches nor loads. */
  private suspended = false;
  private watchPending = false;
  /** No build result has arrived since this view started watching: the pages
   *  it holds were built before and are not known to be current. */
  private awaitingBuild = true;
  private geometrySeq = 0;
  private showSeq = 0;
  /** Newest build this view has accepted for display (shown or loading). */
  private newest: { revision: number; seq: number } | null = null;
  /** Newest build result handled (successful or not). */
  private newestEvent: { revision: number; seq: number } | null = null;
  private recovering: Promise<boolean> | null = null;
  private unlisten: Array<() => void> = [];
  private disposed = false;
  /** Bumped by attach/detach so a superseded attach stops (React remounts). */
  private attachGen = 0;
  private watching = false;
  private editAt: { revision: number; time: number } | null = null;
  private trustPrompted = false;

  constructor(readonly path: string) {}

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getModel = (): PageViewModel => this.model;

  private set(patch: Partial<PageViewModel>): void {
    if (this.disposed) return;
    this.model = { ...this.model, ...patch };
    for (const listener of this.listeners) listener();
  }

  async attach(): Promise<void> {
    const gen = ++this.attachGen;
    this.disposed = false;
    const current = () => gen === this.attachGen && !this.disposed;
    try {
      const unDocuments = await events.documents((e) => this.onDocumentEvent(e));
      const unBuilds = await events.typesettingBuild((e) => void this.onBuild(e));
      const unTrust = await events.trustChanged((change) => void this.onTrustChanged(change));
      if (!current()) {
        unDocuments();
        unBuilds();
        unTrust();
        return;
      }
      this.unlisten.push(unDocuments, unBuilds, unTrust);
      // Open (or join) the document through the workspace, sharing the one
      // document handle with the source editor.
      const handle = await openDocument(this.path);
      const info = await commands.documentState(handle.id);
      if (!current()) return;
      this.docId = handle.id;
      this.handle = handle;
      this.unlisten.push(handle.subscribe(() => this.syncReadOnly()));
      this.syncReadOnly();
      this.docRevision = info.info.revision;
      this.eventRevision = info.info.revision;
      this.set({ phase: "ready", documentId: handle.id });
      // A view that is displayed watches its entry; a hidden one starts when shown.
      if (!this.suspended) await this.startWatching();
    } catch (error) {
      if (current()) this.set({ phase: "error", error: errorMessage(error) });
    }
  }

  /**
   * Count this view as one watcher of its entry (the entry is built now and
   * after every change that concerns it) and show the latest retained build.
   * Those pages were built before the view watched, so they stay marked stale
   * until a build result arrives.
   */
  private async startWatching(): Promise<void> {
    if (this.watching || this.watchPending || !this.docId || this.disposed) return;
    const gen = this.attachGen;
    this.watchPending = true;
    this.awaitingBuild = true;
    if (!this.model.stale) this.set({ stale: true });
    try {
      const latest = await commands.typesettingWatch(this.path);
      if (gen !== this.attachGen || this.disposed || this.suspended) {
        void commands.typesettingUnwatch(this.path).catch(() => {});
        return;
      }
      this.watching = true;
      // A newer build may already have arrived as an event; onBuild keeps
      // whichever is newest.
      if (latest) await this.onBuild(latest, true);
    } catch (error) {
      if (gen === this.attachGen && !this.disposed) this.set({ phase: "error", error: errorMessage(error) });
    } finally {
      if (gen === this.attachGen) this.watchPending = false;
    }
  }

  private stopWatching(): void {
    if (!this.watching) return;
    this.watching = false;
    void commands.typesettingUnwatch(this.path).catch(() => {});
  }

  /**
   * The view is displayed (`true`) or hidden behind another tab (`false`).
   * A hidden view does no work: it stops watching its entry (no builds on its
   * account), ignores builds, and requests no page images or geometry. It
   * keeps following the document's changes, so its caret stays mapped. Shown
   * again, it watches again, which builds the entry; until that result
   * arrives the pages it still holds are marked stale.
   */
  setVisible(visible: boolean): void {
    if (this.suspended === !visible) return;
    this.suspended = !visible;
    if (!visible) {
      // A build that is still loading its pages is not shown.
      this.showSeq += 1;
      this.stopWatching();
      return;
    }
    if (!this.docId || this.disposed) return; // attach() starts watching
    void this.startWatching();
    void this.refreshGeometry();
    void this.loadDisplayed();
  }

  detach(): void {
    this.attachGen += 1;
    this.disposed = true;
    for (const waiter of this.revisionWaiters.splice(0)) waiter.resolve(false);
    for (const un of this.unlisten) un();
    this.unlisten = [];
    this.stopWatching();
    this.watchPending = false;
    for (const url of this.urls.values()) URL.revokeObjectURL(url);
    this.urls.clear();
    this.loading.clear();
  }

  /** Follow the document's read-only state (it changes on normalize/undo). */
  private syncReadOnly(): void {
    const reason = this.handle?.editReason ?? null;
    if (reason !== this.model.readOnly) this.set({ readOnly: reason });
  }

  // ---- builds -------------------------------------------------------------

  /** A build result: an event, or (`retained`) the latest retained build returned when watching started. */
  async onBuild(event: BuildEvent, retained = false): Promise<void> {
    if (event.result.target.entry !== this.path || this.disposed || !this.docId || this.suspended) return;
    if (!retained) this.awaitingBuild = false;
    // Ignore results older than one already handled (events and the watch
    // reply can arrive in either order).
    // A build that compiled nothing (refused for an untrusted workspace, a
    // missing tool) has no source revision: order it by its sequence alone.
    const revision = this.revisionOf(event.result) ?? this.newestEvent?.revision ?? 0;
    const key = { revision, seq: buildSequence(event.result.id) };
    if (this.newestEvent && isOlder(key, this.newestEvent)) return;
    this.newestEvent = key;
    // LaTeX builds run the TeX engine and need a trusted workspace: offer the
    // shared trust prompt (once per view) instead of building. Trust granted
    // or revoked anywhere rebuilds watched entries, so this view follows it;
    // after a revoke (pages already shown) it says so without prompting.
    if (event.refusal?.code === "untrustedWorkspace" && event.refusal.workspaceRoot) {
      this.set({ untrustedRoot: event.refusal.workspaceRoot });
      if (!this.trustPrompted && !this.build) {
        this.trustPrompted = true;
        await this.trust();
      }
      return;
    }
    if (this.model.untrustedRoot) this.set({ untrustedRoot: null });
    if (event.result.status.kind === "succeeded") {
      await this.show(event.result);
      this.set({ failed: false, errorCount: 0 });
    } else {
      if (!this.build && event.lastGood) await this.show(event.lastGood);
      const errors = event.result.diagnostics.filter((d) => d.severity === "error").length;
      this.set({ failed: true, stale: true, errorCount: errors });
    }
  }

  private revisionOf(result: BuildResult): number | null {
    return result.sources.find((s) => s.document === this.docId)?.revision ?? null;
  }

  /** The image of a page, requested once per render reference. */
  private urlFor(renderRef: string): Promise<string> {
    const cached = this.urls.get(renderRef);
    if (cached) {
      // Most recently displayed last.
      this.urls.delete(renderRef);
      this.urls.set(renderRef, cached);
      return Promise.resolve(cached);
    }
    const pending = this.loading.get(renderRef);
    if (pending) return pending;
    const gen = this.attachGen;
    const request = commands
      .pageImage(renderRef)
      .then((buffer) => {
        // A view detached meanwhile holds no images (nothing would revoke this one).
        if (gen !== this.attachGen || this.disposed) throw new Error("the page view was closed");
        const bytes = new Uint8Array(buffer);
        const url = URL.createObjectURL(new Blob([bytes], { type: imageType(bytes) }));
        this.urls.set(renderRef, url);
        return url;
      })
      .finally(() => {
        if (this.loading.get(renderRef) === request) this.loading.delete(renderRef);
      });
    this.loading.set(renderRef, request);
    return request;
  }

  /** The displayed page indexes that exist in a build of `count` pages. */
  private displayedIn(count: number): number[] {
    const within = this.displayed.filter((index) => index < count);
    if (within.length > 0) return within;
    // Nothing reported yet: the first pages. The document became shorter than
    // the displayed position: its last pages.
    const first = this.displayed.length === 0 ? 0 : Math.max(0, count - FIRST_PAGES);
    return Array.from({ length: Math.min(FIRST_PAGES, count) }, (_, i) => first + i);
  }

  /** The shown build's pages with the images held now. */
  private pageImages(result: BuildResult): PageImage[] {
    return result.pages.map((p) => ({
      index: p.index,
      widthPt: p.widthPt,
      heightPt: p.heightPt,
      renderRef: p.renderRef,
      url: this.urls.get(p.renderRef) ?? null,
    }));
  }

  /**
   * Release images the shown build no longer has, and all but the
   * `IDLE_PAGE_IMAGES` most recently displayed images of pages that are not
   * displayed.
   */
  private pruneImages(): void {
    const build = this.build;
    if (!build) return;
    const refs = new Set(build.pages.map((p) => p.renderRef));
    const displayed = new Set(this.displayedIn(build.pages.length).map((index) => build.pages[index]!.renderRef));
    const idle: string[] = [];
    for (const [ref, url] of this.urls) {
      if (!refs.has(ref)) {
        URL.revokeObjectURL(url);
        this.urls.delete(ref);
      } else if (!displayed.has(ref)) {
        idle.push(ref);
      }
    }
    for (const ref of idle.slice(0, Math.max(0, idle.length - IDLE_PAGE_IMAGES))) {
      URL.revokeObjectURL(this.urls.get(ref)!);
      this.urls.delete(ref);
    }
  }

  /**
   * The pages the view displays now (zero-based indexes: its viewport and the
   * pages next to it). Only their images are requested; other pages keep
   * their place and size and get their image when they are displayed.
   */
  setDisplayedPages(indexes: readonly number[]): void {
    const next = [...new Set(indexes)].filter((index) => Number.isInteger(index) && index >= 0).sort((a, b) => a - b);
    if (next.length === this.displayed.length && next.every((index, i) => index === this.displayed[i])) return;
    this.displayed = next;
    void this.loadDisplayed();
  }

  /** Load the images of the displayed pages of the shown build. */
  private async loadDisplayed(): Promise<void> {
    const build = this.build;
    if (!build || this.disposed || this.suspended) return;
    const wanted = this.displayedIn(build.pages.length).map((index) => build.pages[index]!.renderRef);
    await Promise.all(wanted.map((ref) => this.urlFor(ref).catch(() => null)));
    if (this.build !== build || this.disposed) return;
    this.pruneImages();
    const pages = this.pageImages(build);
    if (pages.some((page, i) => page.url !== this.model.pages[i]?.url)) this.set({ pages });
  }

  private async show(result: BuildResult): Promise<void> {
    const revision = this.revisionOf(result) ?? this.docRevision;
    // Never regress to an older build: builds arrive both as events and from
    // `typesetting_watch`, in either order, and display is asynchronous.
    const key = { revision, seq: buildSequence(result.id) };
    if (this.newest && isOlder(key, this.newest)) return;
    this.newest = key;
    const seq = ++this.showSeq;
    // Load the pages the view displays before replacing the shown build, so
    // what is on screen changes in one step; every other page keeps its place
    // and size and loads its image when it is displayed.
    const first = this.displayedIn(result.pages.length);
    await Promise.all(first.map((index) => this.urlFor(result.pages[index]!.renderRef).catch(() => null)));
    if (seq !== this.showSeq || this.disposed || this.suspended) return;
    this.build = result;
    this.buildRevision = revision;
    this.pruneImages();
    const pages = this.pageImages(result);
    // Keep the revision steps the caret and anchor still need: a page edit's
    // reply or a stale retry can leave them older than the shown build.
    this.history.prune(Math.min(revision, this.caret?.revision ?? revision, this.anchor?.revision ?? revision));
    let latencyMs = this.model.latencyMs;
    if (this.editAt && revision >= this.editAt.revision) {
      latencyMs = performance.now() - this.editAt.time;
      this.editAt = null;
    }
    this.set({
      pages,
      buildId: result.id,
      buildRevision: revision,
      stale: this.awaitingBuild || this.docRevision > revision,
      latencyMs,
    });
    await this.refreshGeometry();
    // The view may display other pages by now than when the build arrived.
    void this.loadDisplayed();
  }

  // ---- document events ----------------------------------------------------

  private onDocumentEvent(event: DocumentEvent): void {
    if (event.kind === "closed" && event.document === this.docId) {
      this.set({ phase: "missing", error: `${this.path} was closed` });
      return;
    }
    if (event.kind !== "changed" || event.document !== this.docId) return;
    const change: DocumentChanged = event;
    this.history.add({ previous: change.previousRevision, revision: change.revision, patches: change.patches });
    this.docRevision = Math.max(this.docRevision, change.revision);
    this.eventRevision = Math.max(this.eventRevision, change.revision);
    this.revisionWaiters = this.revisionWaiters.filter((waiter) => {
      if (waiter.revision > this.eventRevision) return true;
      waiter.resolve(true);
      return false;
    });
    const ownChange = change.transaction !== null && this.own.has(change.transaction);
    const restore = change.change !== "edit" && change.origin.kind === "page" ? change.selection?.[0] : undefined;
    if (restore) {
      // Undo/redo from this view restores the recorded selection.
      this.caret = { offset: restore.end, revision: change.revision, affinity: "downstream" };
      this.anchor = restore.start !== restore.end ? { offset: restore.start, revision: change.revision } : null;
    } else if (!ownChange) {
      this.syncPositions();
    }
    this.set({ stale: true });
    // A hidden view maps its caret when it is shown again.
    if (!this.suspended) void this.refreshGeometry();
  }

  /** Bring caret and anchor to the current document revision. */
  private syncPositions(): boolean {
    let ok = true;
    if (this.caret && this.caret.revision < this.docRevision) {
      const mapped = this.history.forward(this.caret.offset, this.caret.revision, this.docRevision, this.caret.affinity);
      if (mapped === null) ok = false;
      else this.caret = { ...this.caret, offset: mapped, revision: this.docRevision };
    }
    if (this.anchor && this.anchor.revision < this.docRevision) {
      const mapped = this.history.forward(this.anchor.offset, this.anchor.revision, this.docRevision, "upstream");
      this.anchor = mapped === null ? null : { offset: mapped, revision: this.docRevision };
    }
    return ok;
  }

  /** An offset of the current caret revision expressed in the displayed build. */
  private toBuild(position: Position): number | null {
    if (position.revision === this.buildRevision) return position.offset;
    if (position.revision > this.buildRevision) {
      return this.history.backward(position.offset, position.revision, this.buildRevision);
    }
    return this.history.forward(position.offset, position.revision, this.buildRevision, "downstream");
  }

  /** Whether a current-revision offset is text the shown build contains (it maps there and back). */
  private inShownBuild(offset: number): boolean {
    if (!this.build) return false;
    if (this.docRevision <= this.buildRevision) return true;
    const at = this.history.backward(offset, this.docRevision, this.buildRevision);
    if (at === null) return false;
    return (["upstream", "downstream"] as const).some(
      (affinity) => this.history.forward(at, this.buildRevision, this.docRevision, affinity) === offset,
    );
  }

  private fromBuild(offset: number, affinity: Affinity): Position | null {
    const mapped = this.history.forward(offset, this.buildRevision, this.docRevision, affinity);
    return mapped === null ? null : { offset: mapped, revision: this.docRevision };
  }

  // ---- geometry -----------------------------------------------------------

  /** Resolves whether the caret has a position on the shown pages. */
  async refreshGeometry(): Promise<boolean> {
    const build = this.build;
    const caret = this.caret;
    if (!build || !caret || this.disposed) {
      this.set({ caret: null, selection: [], selectionText: "", hasSelection: false });
      return false;
    }
    const seq = ++this.geometrySeq;
    const offset = this.toBuild(caret);
    const anchor = this.anchor ? this.toBuild(this.anchor) : null;
    try {
      const geometry = offset === null ? null : await commands.pageCaret(build.id, this.path, offset, caret.affinity);
      let selection: PageRect[] = [];
      let selectionText = "";
      if (anchor !== null && offset !== null && anchor !== offset) {
        const result = await commands.pageSelection(build.id, this.path, Math.min(anchor, offset), Math.max(anchor, offset));
        selection = result.rects;
        selectionText = result.text;
      }
      if (seq !== this.geometrySeq) return geometry !== null;
      this.set({
        caret: geometry,
        selection,
        selectionText,
        hasSelection: this.anchor !== null && this.anchor.offset !== caret.offset,
      });
      return geometry !== null;
    } catch (error) {
      // The shown build is no longer retained: fetch a current one.
      if (isUnknownBuild(error)) void this.recover();
      return false;
    }
  }

  /** The shown build was evicted: build now and show the result. */
  private recover(): Promise<boolean> {
    this.recovering ??= (async () => {
      try {
        const event = await commands.typesettingBuild(this.path);
        if (this.disposed) return false;
        this.newest = null;
        this.newestEvent = null;
        await this.onBuild(event);
        return event.result.status.kind === "succeeded";
      } catch {
        return false;
      } finally {
        this.recovering = null;
      }
    })();
    return this.recovering;
  }

  // ---- pointer ------------------------------------------------------------

  private staleRefusal(): boolean {
    if (!this.model.failed) return false;
    this.set({
      message: {
        tone: "warning",
        text: "These pages are from an earlier revision (the document has errors): fix them first, or edit in source",
      },
    });
    return true;
  }

  async click(page: number, x: number, y: number, extend: boolean, retried = false): Promise<void> {
    const build = this.build;
    if (!build || this.staleRefusal()) return;
    let hit;
    try {
      hit = await commands.pageHitTest(build.id, page, x, y);
    } catch (error) {
      // The shown build was evicted: rebuild, then place the caret once more.
      if (isUnknownBuild(error) && !retried && (await this.recover())) return this.click(page, x, y, extend, true);
      this.set({ message: { tone: "warning", text: errorMessage(error) } });
      return;
    }
    const message = hit.nonEditable ? { tone: "info" as const, text: NON_EDITABLE[hit.nonEditable] } : null;
    if (!hit.caret || hit.caret.path !== this.path) {
      this.set({
        message: hit.caret ? { tone: "info", text: `In ${hit.caret.path} — open that file to edit it` } : message,
      });
      return;
    }
    const position = this.fromBuild(hit.caret.offset, hit.caret.affinity);
    if (!position) return;
    if (extend) {
      if (!this.anchor && this.caret) this.anchor = { offset: this.caret.offset, revision: this.caret.revision };
    } else {
      this.anchor = null;
    }
    this.caret = { ...position, affinity: hit.caret.affinity };
    this.unapplied = null;
    this.goalX = null;
    this.group += 1;
    this.set({ message });
    await this.refreshGeometry();
  }

  // ---- keyboard navigation --------------------------------------------------

  async move(motion: Motion, extend: boolean): Promise<void> {
    await this.enqueue(async () => {
      const build = this.build;
      if (!build || this.staleRefusal()) return;
      this.syncPositions();
      if (!this.caret) {
        // No caret yet (keyboard user): start at the beginning of the document.
        this.caret = { offset: 0, revision: this.docRevision, affinity: "downstream" };
        motion = "documentStart";
        extend = false;
      }
      const caret = this.caret;
      // Collapse a selection to its edge on plain left/right.
      if (!extend && this.anchor && (motion === "left" || motion === "right")) {
        const anchor = this.anchor;
        const forward = motion === "right";
        const edge = forward === anchor.offset > caret.offset ? anchor.offset : caret.offset;
        this.caret = { ...caret, offset: edge };
        this.anchor = null;
        this.group += 1;
        await this.refreshGeometry();
        return;
      }
      const offset = this.toBuild(caret);
      if (offset === null) return;
      const vertical = motion === "up" || motion === "down";
      let result;
      try {
        result = await commands.pageNavigate(build.id, this.path, offset, caret.affinity, motion, vertical ? this.goalX : null);
      } catch (error) {
        if (isUnknownBuild(error)) void this.recover();
        return;
      }
      if (!result || result.caret.path !== this.path) return;
      const position = this.fromBuild(result.caret.offset, result.caret.affinity);
      if (!position) return;
      if (extend) {
        if (!this.anchor) this.anchor = { offset: caret.offset, revision: caret.revision };
      } else {
        this.anchor = null;
      }
      this.caret = { ...position, affinity: result.caret.affinity };
      this.goalX = vertical ? result.goalX : null;
      this.group += 1;
      await this.refreshGeometry();
    });
  }

  async collapse(): Promise<void> {
    this.anchor = null;
    await this.refreshGeometry();
  }

  async selectAll(): Promise<void> {
    await this.move("documentStart", false);
    await this.move("documentEnd", true);
  }

  // ---- editing ------------------------------------------------------------

  private enqueue(task: () => Promise<void>): Promise<void> {
    const run = this.queue.then(task, task);
    this.queue = run.catch(() => {});
    return run;
  }

  edit(op: PageEditOp): Promise<void> {
    this.editAt ??= { revision: this.docRevision + 1, time: performance.now() };
    return this.enqueue(() => this.sendEdit(op, 0));
  }

  private async sendEdit(op: PageEditOp, attempt: number): Promise<void> {
    if (!this.docId) return;
    // The document service would refuse it: say why instead of trying.
    if (this.model.readOnly) {
      this.set({ message: { tone: "warning", text: this.model.readOnly } });
      return;
    }
    // Typing is paused after a refusal: keep collecting (never apply at a
    // guessed position) until the user places the caret again.
    if (this.unapplied !== null) {
      this.refuseTyping(op);
      return;
    }
    if (!this.syncPositions()) {
      // A change the caret must be mapped over has not been received yet:
      // wait for it; refuse explicitly only if it never arrives.
      if (!(await this.waitForEvents(this.docRevision, EVENT_WAIT_MS)) || !this.syncPositions()) {
        this.refuseTyping(op);
        return;
      }
    }
    const caret = this.caret;
    if (!caret) {
      this.set({ message: { tone: "info", text: "Click in the text to place the caret" } });
      return;
    }
    const anchor = this.anchor && this.anchor.revision === caret.revision ? this.anchor.offset : caret.offset;
    const id = newId("page");
    this.own.add(id);
    try {
      const outcome = await commands.pageEdit({
        id,
        document: this.docId,
        baseRevision: caret.revision,
        selection: { start: anchor, end: caret.offset },
        op,
        group: this.group,
      });
      switch (outcome.kind) {
        case "applied":
          this.docRevision = Math.max(this.docRevision, outcome.revision);
          this.caret = this.landCaret(outcome.caret, outcome.revision);
          this.anchor = null;
          this.goalX = null;
          this.set({ message: null, stale: true });
          break;
        case "unchanged":
          this.caret = this.landCaret(outcome.caret, outcome.revision);
          this.anchor = null;
          break;
        case "refused": {
          const text = refusalText(outcome.refusal);
          this.set({ message: text ? { tone: "warning", text } : null });
          break;
        }
        case "stale":
          // A concurrent change landed that this view has not received yet:
          // wait for its change event, map the caret over it, and send the
          // keystroke again. Each retry waits for a newer revision, so it
          // makes progress; without the event the keystroke is refused
          // explicitly instead of being dropped.
          if (attempt < MAX_STALE_RETRIES && (await this.waitForEvents(outcome.currentRevision, EVENT_WAIT_MS))) {
            await this.sendEdit(op, attempt + 1);
          } else {
            this.refuseTyping(op);
          }
          return;
      }
    } catch (error) {
      this.set({ message: { tone: "warning", text: errorMessage(error) } });
    } finally {
      setTimeout(() => this.own.delete(id), 5000);
    }
    await this.refreshGeometry();
  }

  /** The caret after an edit applied as `revision`. Change events of newer,
   *  concurrent edits may have arrived before the reply: express the caret in
   *  the newest received revision instead of moving it back. */
  private landCaret(offset: number, revision: number): Position & { affinity: Affinity } {
    if (this.eventRevision > revision) {
      const mapped = this.history.forward(offset, revision, this.eventRevision, "downstream");
      if (mapped !== null) return { offset: mapped, revision: this.eventRevision, affinity: "downstream" };
    }
    return { offset, revision, affinity: "downstream" };
  }

  /** Resolves true once change events up to `revision` were received. */
  private waitForEvents(revision: number, timeoutMs: number): Promise<boolean> {
    if (this.eventRevision >= revision) return Promise.resolve(true);
    return new Promise((resolve) => {
      const waiter = { revision, resolve };
      this.revisionWaiters.push(waiter);
      setTimeout(() => {
        const i = this.revisionWaiters.indexOf(waiter);
        if (i >= 0) {
          this.revisionWaiters.splice(i, 1);
          resolve(false);
        }
      }, timeoutMs);
    });
  }

  /** Refuse a page edit explicitly, keeping its text visible as unapplied. */
  private refuseTyping(op: PageEditOp): void {
    const text = op.kind === "insert" ? op.text : op.kind === "paragraphBreak" ? "\u21b5" : "";
    this.unapplied = (this.unapplied ?? "") + text;
    const what = this.unapplied ? `“${this.unapplied}” was not applied` : "an edit was not applied";
    this.set({
      message: {
        tone: "warning",
        text: `Typing paused: the document changed under the caret, so ${what}. Click to place the caret and type again.`,
      },
    });
  }

  async undo(redo = false): Promise<void> {
    await this.enqueue(async () => {
      if (!this.docId) return;
      try {
        const change = redo
          ? await commands.documentRedo(this.docId, { kind: "page" })
          : await commands.documentUndo(this.docId, { kind: "page" });
        this.docRevision = Math.max(this.docRevision, change.revision);
        const r = change.selection?.[0];
        if (r) {
          this.caret = { offset: r.end, revision: change.revision, affinity: "downstream" };
          this.anchor = r.start !== r.end ? { offset: r.start, revision: change.revision } : null;
        }
        this.group += 1;
      } catch (error) {
        this.set({ message: { tone: "info", text: errorMessage(error) } });
      }
      await this.refreshGeometry();
    });
  }

  setComposition(text: string): void {
    this.set({ composition: text });
  }

  showMessage(message: PageMessage | null): void {
    this.set({ message });
  }

  /**
   * Place the caret at a source offset of the current revision (problems
   * panel). Resolves whether the shown pages can show that position: false
   * when there are no pages or the position is not on them (e.g. source added
   * after the last good build), so the caller can show it in the source.
   */
  async reveal(offset: number): Promise<boolean> {
    this.caret = { offset, revision: this.docRevision, affinity: "downstream" };
    this.unapplied = null;
    this.anchor = null;
    this.group += 1;
    // Text added after the shown build has no place on its pages: mapped back
    // it lands at the insertion point (a caret there points at the wrong
    // text, e.g. the end of the last page for an error appended after it).
    const onPages = this.inShownBuild(offset);
    const shown = await this.refreshGeometry();
    return shown && onPages && !this.disposed;
  }

  /**
   * Ask to trust the workspace. A grant (here or on any other surface) is
   * announced as a trust change, on which the app rebuilds the watched LaTeX
   * entries; that build replaces the refusal, so this view requests nothing.
   * Only a folder that is trusted already (nothing to announce) gets its
   * build requested here, once.
   */
  async trust(): Promise<boolean> {
    const root = this.model.untrustedRoot;
    if (!root) return true;
    const status = await commands.trustStatus(root).catch(() => null);
    if (status?.trusted) {
      await this.requestBuild();
      return true;
    }
    return requestWorkspaceTrust(root);
  }

  /**
   * Trust granted or revoked anywhere. The app rebuilds the watched entries
   * itself; this view only stops showing the refusal as soon as its folder is
   * trusted (the build then shows pages). A revoke comes back as a refused build.
   */
  private async onTrustChanged(change: TrustChanged | null): Promise<void> {
    const root = this.model.untrustedRoot;
    if (!root || this.disposed || change?.granted === false) return;
    const status = await commands.trustStatus(root).catch(() => null);
    if (status?.trusted && this.model.untrustedRoot === root) this.set({ untrustedRoot: null });
  }

  /** Request this view's build again (it already holds its one watch). */
  private async requestBuild(): Promise<void> {
    if (this.disposed) return;
    this.set({ untrustedRoot: null });
    await commands.typesettingWatch(this.path).catch(() => null);
    await commands.typesettingUnwatch(this.path).catch(() => null);
  }

  /**
   * Export the build shown on this view's pages to a PDF file the person
   * chooses in the native save dialog (opened by the backend, starting next
   * to the document). The export is bound to this document and that build;
   * the source is neither saved nor changed. `null` when the dialog was
   * canceled (nothing was written).
   */
  async exportPdf(): Promise<PdfExport | null> {
    const build = this.build;
    if (!build) throw new Error("Nothing to export yet");
    let path: string;
    try {
      path = (await commands.typesettingExportPdf(build.id, this.path)).path;
    } catch (error) {
      if (isCommandError(error) && error.code === "typesetting.exportCanceled") return null;
      throw error;
    }
    // The file holds the build shown when the export started. Say so when
    // that is not the current source: the latest build failed (the last good
    // pages stay visible), or the source changed since (also while the
    // dialog was open).
    const outdated = this.model.failed ? "failed" : this.build?.id !== build.id || this.model.stale ? "changed" : null;
    return { path, outdated };
  }

  /**
   * The caret and selection anchor in the document's current revision, for
   * tools that write source at the caret (the equation editor; see
   * `activePage.ts`). Null when there is no caret, typing is paused after a
   * refusal, or the caret cannot be mapped to the current revision (a change
   * it must be mapped over has not been received yet): never an older
   * revision's offset.
   */
  currentCaret(): { offset: number; anchor: number | null; revision: number; mode: EditMode | null } | null {
    if (!this.caret || this.unapplied !== null || this.disposed) return null;
    if (!this.syncPositions() || this.caret.revision !== this.docRevision) return null;
    const anchor =
      this.anchor && this.anchor.revision === this.caret.revision && this.anchor.offset !== this.caret.offset
        ? this.anchor.offset
        : null;
    return { offset: this.caret.offset, anchor, revision: this.caret.revision, mode: this.model.caret?.mode ?? null };
  }

  /** Test/automation hook: the caret in the current revision. */
  caretPosition(): { offset: number; revision: number; anchor: number | null } | null {
    if (!this.caret) return null;
    return { offset: this.caret.offset, revision: this.caret.revision, anchor: this.anchor?.offset ?? null };
  }
}
