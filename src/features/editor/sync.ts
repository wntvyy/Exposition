// Reconciles one editor view (a Monaco model) with the document service.
//
// The service is the only authority. The view applies local edits
// optimistically and sends each as a transaction through a serial queue (one
// request in flight, each based on the latest revision the view has seen,
// `RebaseDisjoint`). Service events arrive in revision order: the view's own
// transaction is an acknowledgement; every other revision (other origins,
// undo/redo) is transformed over the unacknowledged local edits and applied to
// the view without echo. A rejected transaction, a gap in revisions, or an
// overlap the view cannot transform triggers a resync from `document_state`.
import type { DocumentChanged } from "../../bindings/DocumentChanged";
import type { DocumentId } from "../../bindings/DocumentId";
import type { DocumentState } from "../../bindings/DocumentState";
import type { SelectionHint } from "../../bindings/SelectionHint";
import type { TextPatch } from "../../bindings/TextPatch";
import type { TextRange } from "../../bindings/TextRange";
import type { TransactionOutcome } from "../../bindings/TransactionOutcome";
import type { TransactionRequest } from "../../bindings/TransactionRequest";
import type { UndoBehavior } from "../../bindings/UndoBehavior";
import { samePatches, transform } from "./ot";

export interface SyncTransport {
  apply(request: TransactionRequest): Promise<TransactionOutcome>;
  undo(document: DocumentId): Promise<unknown>;
  redo(document: DocumentId): Promise<unknown>;
  state(document: DocumentId): Promise<DocumentState>;
}

export interface SyncView {
  /** Apply sorted patches relative to the current view text, without echo or view-level undo. */
  applyPatches(patches: TextPatch[]): void;
  /** Can these patches be applied exactly (e.g. without line-ending normalization)? */
  canApply(patches: TextPatch[]): boolean;
  /** Replace the view text with the authoritative text. */
  setText(text: string): void;
  /** Adopt a selection restored by undo/redo. */
  setSelection?(ranges: TextRange[]): void;
}

export interface SyncCallbacks {
  /** Revision, pending state, or resync status changed. */
  onChange?(): void;
  /** A local edit was refused by the service; the view was resynchronized. */
  onRejected?(error: unknown): void;
}

interface PendingEdit {
  kind: "edit";
  id: string;
  /** Relative to the text after everything before it in the queue. */
  patches: TextPatch[];
  undo: UndoBehavior;
  selection: SelectionHint | null;
  sent: boolean;
}

interface PendingHistory {
  kind: "undo" | "redo";
  sent: boolean;
}

type Pending = PendingEdit | PendingHistory;

let transactionCounter = 0;
const newTransactionId = () =>
  `src-${globalThis.crypto?.randomUUID?.() ?? `${Date.now().toString(36)}-${(transactionCounter++).toString(36)}`}`;

export class DocumentSync {
  /** Last service revision applied to the view. */
  revision: number;
  private queue: Pending[] = [];
  private resyncing: Promise<void> | null = null;
  private held: DocumentChanged[] = [];
  private idleWaiters: (() => void)[] = [];
  private disposed = false;

  constructor(
    readonly document: DocumentId,
    revision: number,
    private readonly transport: SyncTransport,
    private readonly view: SyncView,
    private readonly callbacks: SyncCallbacks = {},
  ) {
    this.revision = revision;
  }

  /** Local edits not yet acknowledged by the service. */
  get pendingCount(): number {
    return this.queue.length;
  }

  get isResyncing(): boolean {
    return this.resyncing !== null;
  }

  dispose() {
    this.disposed = true;
    this.queue = [];
    this.flushWaiters();
  }

  /** Record a local edit already applied to the view (patches relative to the text before it). */
  local(patches: TextPatch[], undo: UndoBehavior, selection: SelectionHint | null = null) {
    if (this.disposed || patches.length === 0) return;
    this.queue.push({ kind: "edit", id: newTransactionId(), patches, undo, selection, sent: false });
    this.callbacks.onChange?.();
    this.pump();
  }

  /** Queue an undo/redo of the shared history, ordered after pending local edits. */
  history(kind: "undo" | "redo") {
    if (this.disposed) return;
    this.queue.push({ kind, sent: false });
    this.pump();
  }

  /** Resolves once every queued local edit is acknowledged (or dropped by a resync). */
  idle(): Promise<void> {
    if (this.queue.length === 0 && !this.resyncing) return Promise.resolve();
    return new Promise((resolve) => this.idleWaiters.push(resolve));
  }

  private flushWaiters() {
    if (this.queue.length === 0 && !this.resyncing) {
      const waiters = this.idleWaiters;
      this.idleWaiters = [];
      for (const w of waiters) w();
    }
  }

  private pump() {
    if (this.disposed || this.resyncing) return;
    const head = this.queue[0];
    if (!head || head.sent) {
      this.flushWaiters();
      return;
    }
    head.sent = true;
    if (head.kind === "edit") {
      const request: TransactionRequest = {
        id: head.id,
        origin: { kind: "source" },
        edits: [{ document: this.document, baseRevision: this.revision, patches: head.patches }],
        undo: head.undo,
        conflict: "rebaseDisjoint",
        label: null,
        selection: head.selection,
      };
      this.transport.apply(request).then(
        () => {
          // Acknowledged by the matching event (which may arrive before or after this).
        },
        (error: unknown) => {
          if (this.queue[0] === head) {
            this.callbacks.onRejected?.(error);
            void this.resync();
          }
        },
      );
    } else {
      const call = head.kind === "undo" ? this.transport.undo(this.document) : this.transport.redo(this.document);
      // The resulting revision arrives as an ordinary event (no transaction id),
      // transformed over any later local edits like any other remote change.
      const done = () => {
        if (this.queue[0] === head) {
          this.queue.shift();
          this.callbacks.onChange?.();
          this.pump();
        }
      };
      call.then(done, done);
    }
  }

  /** Handle a service event for this document (events arrive in revision order). */
  receive(event: DocumentChanged) {
    if (this.disposed) return;
    if (this.resyncing) {
      this.held.push(event);
      return;
    }
    if (event.revision <= this.revision) return;
    if (event.previousRevision !== this.revision) {
      void this.resync();
      return;
    }
    const head = this.queue[0];
    if (head?.kind === "edit" && head.sent && event.transaction === head.id) {
      this.queue.shift();
      this.revision = event.revision;
      if (!samePatches(event.patches, head.patches)) {
        // The service rebased differently than this view; trust the service.
        void this.resync();
        return;
      }
      this.callbacks.onChange?.();
      this.pump();
      return;
    }
    // A revision from elsewhere: transform it over every unacknowledged local edit.
    let remote = event.patches;
    for (const pending of this.queue) {
      if (pending.kind !== "edit") continue;
      const result = transform(remote, pending.patches);
      if (!result) {
        void this.resync();
        return;
      }
      remote = result.remote;
      pending.patches = result.local;
    }
    if (!this.view.canApply(remote)) {
      void this.resync();
      return;
    }
    this.view.applyPatches(remote);
    this.revision = event.revision;
    if (event.selection && event.change !== "edit" && this.queue.length === 0) {
      this.view.setSelection?.(event.selection);
    }
    this.callbacks.onChange?.();
  }

  /** Replace the view with the service's current state, dropping unacknowledged local edits. */
  resync(): Promise<void> {
    if (this.resyncing) return this.resyncing;
    this.queue = [];
    this.held = [];
    const run = (async () => {
      this.callbacks.onChange?.();
      try {
        const state = await this.transport.state(this.document);
        if (this.disposed) return;
        // Edits made while the state was loading are replaced along with the view text.
        this.queue = [];
        this.view.setText(state.text);
        this.revision = state.info.revision;
      } catch {
        // The document was closed or the service is unavailable; nothing to show.
      }
    })().finally(() => {
      this.resyncing = null;
      const held = this.held;
      this.held = [];
      for (const event of held) this.receive(event);
      this.callbacks.onChange?.();
      this.pump();
      this.flushWaiters();
    });
    this.resyncing = run;
    return run;
  }
}
