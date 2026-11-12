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
import type { EditRefusal } from "../../bindings/EditRefusal";
import type { Motion } from "../../bindings/Motion";
import type { NonEditableReason } from "../../bindings/NonEditableReason";
import type { PageEditOp } from "../../bindings/PageEditOp";
import type { PageRect } from "../../bindings/PageRect";
import { commands, errorMessage, events, newId } from "../../lib/ipc";
import { openDocument } from "../editor";
import { requestWorkspaceTrust } from "../jobs/trust";
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
}

interface Position {
  offset: number;
  revision: number;
}

const NON_EDITABLE: Record<NonEditableReason, string> = {
  generated: "Generated content (numbering, markers, captions labels) — edit in source",
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
  };
  private listeners = new Set<() => void>();
  private docId: string | null = null;
  private docRevision = 0;
  private build: BuildResult | null = null;
  private buildRevision = 0;
  private caret: (Position & { affinity: Affinity }) | null = null;
  private anchor: Position | null = null;
  private goalX: number | null = null;
  private group = 1;
  private readonly history = new RevisionHistory();
  private readonly own = new Set<string>();
  private queue: Promise<void> = Promise.resolve();
  private readonly urls = new Map<string, string>();
  private geometrySeq = 0;
  private showSeq = 0;
  private unlisten: Array<() => void> = [];
  private disposed = false;
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
    try {
      this.unlisten.push(await events.documents((e) => this.onDocumentEvent(e)));
      this.unlisten.push(await events.typesettingBuild((e) => void this.onBuild(e)));
      // Open (or join) the document through the workspace, sharing the one
      // document handle with the source editor.
      const handle = await openDocument(this.path);
      const info = await commands.documentState(handle.id);
      this.docId = handle.id;
      this.docRevision = info.info.revision;
      this.set({ phase: "ready", documentId: handle.id });
      const latest = await commands.typesettingWatch(this.path);
      if (latest) await this.onBuild(latest);
    } catch (error) {
      this.set({ phase: "error", error: errorMessage(error) });
    }
  }

  detach(): void {
    this.disposed = true;
    for (const un of this.unlisten) un();
    this.unlisten = [];
    void commands.typesettingUnwatch(this.path).catch(() => {});
    for (const url of this.urls.values()) URL.revokeObjectURL(url);
    this.urls.clear();
  }

  // ---- builds -------------------------------------------------------------

  async onBuild(event: BuildEvent): Promise<void> {
    if (event.result.target.entry !== this.path || this.disposed) return;
    // LaTeX builds run the TeX engine and need a trusted workspace: offer the
    // shared trust prompt (once per view) instead of building.
    if (event.refusal?.code === "untrustedWorkspace" && event.refusal.workspaceRoot) {
      this.set({ untrustedRoot: event.refusal.workspaceRoot });
      if (!this.trustPrompted) {
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

  private async urlFor(renderRef: string): Promise<string> {
    const cached = this.urls.get(renderRef);
    if (cached) return cached;
    const bytes = new Uint8Array(await commands.pageImage(renderRef));
    const url = URL.createObjectURL(new Blob([bytes], { type: imageType(bytes) }));
    this.urls.set(renderRef, url);
    return url;
  }

  private async show(result: BuildResult): Promise<void> {
    const revision = this.revisionOf(result) ?? this.docRevision;
    if (this.build && revision < this.buildRevision) return;
    const seq = ++this.showSeq;
    const pages: PageImage[] = await Promise.all(
      result.pages.map(async (p) => ({
        index: p.index,
        widthPt: p.widthPt,
        heightPt: p.heightPt,
        renderRef: p.renderRef,
        url: await this.urlFor(p.renderRef).catch(() => null),
      })),
    );
    if (seq !== this.showSeq || this.disposed) return;
    this.build = result;
    this.buildRevision = revision;
    const keep = new Set(result.pages.map((p) => p.renderRef));
    for (const [ref, url] of this.urls) {
      if (!keep.has(ref)) {
        URL.revokeObjectURL(url);
        this.urls.delete(ref);
      }
    }
    this.history.prune(revision);
    let latencyMs = this.model.latencyMs;
    if (this.editAt && revision >= this.editAt.revision) {
      latencyMs = performance.now() - this.editAt.time;
      this.editAt = null;
    }
    this.set({ pages, buildId: result.id, buildRevision: revision, stale: this.docRevision > revision, latencyMs });
    await this.refreshGeometry();
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
    void this.refreshGeometry();
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

  private fromBuild(offset: number, affinity: Affinity): Position | null {
    const mapped = this.history.forward(offset, this.buildRevision, this.docRevision, affinity);
    return mapped === null ? null : { offset: mapped, revision: this.docRevision };
  }

  // ---- geometry -----------------------------------------------------------

  async refreshGeometry(): Promise<void> {
    const build = this.build;
    const caret = this.caret;
    if (!build || !caret || this.disposed) {
      this.set({ caret: null, selection: [], selectionText: "", hasSelection: false });
      return;
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
      if (seq !== this.geometrySeq) return;
      this.set({
        caret: geometry,
        selection,
        selectionText,
        hasSelection: this.anchor !== null && this.anchor.offset !== caret.offset,
      });
    } catch {
      // The build was evicted; the next build event refreshes geometry.
    }
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

  async click(page: number, x: number, y: number, extend: boolean): Promise<void> {
    const build = this.build;
    if (!build || this.staleRefusal()) return;
    const hit = await commands.pageHitTest(build.id, page, x, y);
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
      const result = await commands.pageNavigate(build.id, this.path, offset, caret.affinity, motion, vertical ? this.goalX : null);
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
    this.syncPositions();
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
          this.caret = { offset: outcome.caret, revision: outcome.revision, affinity: "downstream" };
          this.anchor = null;
          this.goalX = null;
          this.set({ message: null, stale: true });
          break;
        case "unchanged":
          this.caret = { offset: outcome.caret, revision: outcome.revision, affinity: "downstream" };
          this.anchor = null;
          break;
        case "refused": {
          const text = refusalText(outcome.refusal);
          this.set({ message: text ? { tone: "warning", text } : null });
          break;
        }
        case "stale":
          // Our offsets refer to an older revision: wait for the change events,
          // map the caret forward, and retry once.
          this.docRevision = Math.max(this.docRevision, outcome.currentRevision);
          if (attempt < 3) {
            await new Promise((r) => setTimeout(r, 15 * (attempt + 1)));
            await this.sendEdit(op, attempt + 1);
          } else {
            this.set({ message: { tone: "warning", text: "The document changed; click to place the caret again" } });
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

  /** Place the caret at a source offset of the current revision (problems panel). */
  async reveal(offset: number): Promise<void> {
    this.caret = { offset, revision: this.docRevision, affinity: "downstream" };
    this.anchor = null;
    this.group += 1;
    await this.refreshGeometry();
  }

  /** Ask to trust the workspace; when granted, build again. */
  async trust(): Promise<boolean> {
    const root = this.model.untrustedRoot;
    if (!root) return true;
    const trusted = await requestWorkspaceTrust(root);
    if (trusted && !this.disposed) {
      this.set({ untrustedRoot: null });
      // Request the build again; watches are counted per view, and this view
      // already holds one from when it opened.
      await commands.typesettingWatch(this.path).catch(() => null);
      await commands.typesettingUnwatch(this.path).catch(() => null);
    }
    return trusted;
  }

  async exportPdf(): Promise<string> {
    const build = this.build;
    if (!build) throw new Error("Nothing to export yet");
    const artifact = await commands.typesettingExportPdf(build.id);
    return artifact.path;
  }

  /** Test/automation hook: the caret in the current revision. */
  caretPosition(): { offset: number; revision: number; anchor: number | null } | null {
    if (!this.caret) return null;
    return { offset: this.caret.offset, revision: this.caret.revision, anchor: this.anchor?.offset ?? null };
  }
}
