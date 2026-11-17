// Test doubles for the equation panel: a text model with Monaco's change-event
// and position semantics, and a document wired like `DocumentHandle` — every
// model change goes through the real `UndoGrouper` and `DocumentSync`, so a
// test sees the exact transactions the document service would receive.
import type { TransactionRequest } from "../../../../bindings/TransactionRequest";
import { DocumentSync } from "../../../editor/sync";
import { UndoGrouper } from "../../../editor/typing";
import type { EditableModel, EquationDocument } from "../document";
import type { ContentChange } from "../tracking";

type Listener = (event: { changes: readonly ContentChange[]; isFlush: boolean }) => void;

export class FakeModel implements EditableModel {
  private listeners = new Set<Listener>();
  private disposed = false;
  applyEditsCalls = 0;

  constructor(private text: string) {}

  get listenerCount(): number {
    return this.listeners.size;
  }

  onDidChangeContent(listener: Listener) {
    this.listeners.add(listener);
    return { dispose: () => void this.listeners.delete(listener) };
  }

  getValue(): string {
    return this.text;
  }

  isDisposed(): boolean {
    return this.disposed;
  }

  dispose(): void {
    this.disposed = true;
  }

  getPositionAt(offset: number) {
    const before = this.text.slice(0, offset).split("\n");
    return { lineNumber: before.length, column: before[before.length - 1]!.length + 1 };
  }

  private offsetAt(lineNumber: number, column: number): number {
    const lines = this.text.split("\n");
    let offset = 0;
    for (let i = 0; i < lineNumber - 1; i++) offset += lines[i]!.length + 1;
    return offset + column - 1;
  }

  getOffsetAt(position: { lineNumber: number; column: number }): number {
    return this.offsetAt(position.lineNumber, position.column);
  }

  getValueLength(): number {
    return this.text.length;
  }

  applyEdits(operations: { range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number }; text: string }[]) {
    this.applyEditsCalls += 1;
    const changes = operations.map((op) => {
      const start = this.offsetAt(op.range.startLineNumber, op.range.startColumn);
      const end = this.offsetAt(op.range.endLineNumber, op.range.endColumn);
      return { rangeOffset: start, rangeLength: end - start, text: op.text };
    });
    this.edit(changes);
  }

  /** Apply changes (offsets relative to the text before them) as one change event. */
  edit(changes: ContentChange[]): void {
    const sorted = [...changes].sort((a, b) => b.rangeOffset - a.rangeOffset);
    for (const c of sorted) this.text = this.text.slice(0, c.rangeOffset) + c.text + this.text.slice(c.rangeOffset + c.rangeLength);
    for (const l of [...this.listeners]) l({ changes, isFlush: false });
  }

  flush(text: string): void {
    this.text = text;
    for (const l of [...this.listeners]) l({ changes: [], isFlush: true });
  }
}

export interface FakeDocument extends EquationDocument {
  readonly model: FakeModel;
  /** Transactions sent to the (fake) document service, in order. */
  readonly requests: TransactionRequest[];
  closed: boolean;
  editReason: string | null;
}

/** A document whose model changes become transactions exactly as in `DocumentHandle.onLocalChange`. */
export function fakeDocument(text: string, options: { id?: string; path?: string } = {}): FakeDocument {
  const id = options.id ?? "doc-1";
  const path = options.path ?? "main.tex";
  const model = new FakeModel(text);
  const requests: TransactionRequest[] = [];
  const grouper = new UndoGrouper();
  const sync = new DocumentSync(
    id,
    1,
    {
      apply: async (request) => {
        requests.push(request);
        // The service applies it and announces the new revision.
        const previous = sync.revision;
        queueMicrotask(() =>
          sync.receive({
            kind: "changed",
            document: id,
            transaction: request.id,
            origin: request.origin,
            change: "edit",
            previousRevision: previous,
            revision: previous + 1,
            patches: request.edits[0]!.patches,
            selection: null,
            canUndo: true,
            canRedo: false,
          } as never),
        );
        return {} as never;
      },
      undo: async () => {},
      redo: async () => {},
      state: async () => ({ info: { revision: 1 } as never, text: model.getValue() }),
    },
    { applyPatches: () => {}, canApply: () => true, setText: () => {}, getText: () => model.getValue() },
  );
  const doc: FakeDocument = {
    id,
    path,
    name: path.split("/").pop()!,
    closed: false,
    editReason: null,
    model,
    sync,
    grouper,
    selection: [],
    requests,
  };
  model.onDidChangeContent((e) => {
    if (e.isFlush) return;
    const patches = [...e.changes]
      .sort((a, b) => a.rangeOffset - b.rangeOffset)
      .map((c) => ({ range: { start: c.rangeOffset, end: c.rangeOffset + c.rangeLength }, text: c.text }));
    const undo = grouper.classify(patches, Date.now());
    sync.local(patches, undo, { before: doc.selection, after: [] });
  });
  return doc;
}
