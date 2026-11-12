// A page view of a document the service will not edit (mixed line endings)
// shows the same read-only reason as the source editor, refuses typing
// without sending it, and follows the document when it is normalized.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const MIXED = "This file mixes line endings. Normalize them to edit it.";

const doc = vi.hoisted(() => {
  const listeners = new Set<() => void>();
  return {
    handle: {
      id: "doc-1",
      editReason: null as string | null,
      subscribe(listener: () => void) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
    },
    /** The document's state changed (as a DocumentHandle notifies). */
    change(reason: string | null) {
      doc.handle.editReason = reason;
      listeners.forEach((l) => l());
    },
    pageEdits: 0,
  };
});

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: 1 }, text: "Hello\r\nworld\n" }),
    typesettingWatch: async () => null,
    typesettingUnwatch: async () => {},
    pageEdit: async () => {
      doc.pageEdits += 1;
      return { kind: "refused", revision: 1, refusal: { kind: "noChange" } };
    },
    pageCaret: async () => null,
  },
  events: {
    documents: async () => () => {},
    typesettingBuild: async () => () => {},
    trustChanged: async () => () => {},
  },
  errorMessage: (e: unknown) => String(e),
  isCommandError: () => false,
  newId: (prefix: string) => `${prefix}-1`,
}));
vi.mock("../editor", () => ({ openDocument: async () => doc.handle }));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn() }));

import { PageSession } from "./session";

describe("page view of a document with mixed line endings", () => {
  let session: PageSession;

  beforeEach(async () => {
    doc.handle.editReason = MIXED;
    doc.pageEdits = 0;
    session = new PageSession("main.typ");
    await session.attach();
  });

  afterEach(() => session.detach());

  it("shows the read-only reason and refuses typing without sending it", async () => {
    expect(session.getModel().readOnly).toBe(MIXED);
    await session.edit({ kind: "insert", text: "x" });
    expect(doc.pageEdits).toBe(0);
    expect(session.getModel().message).toEqual({ tone: "warning", text: MIXED });
  });

  it("becomes editable once the document is normalized, and read-only again after undoing that", async () => {
    doc.change(null);
    expect(session.getModel().readOnly).toBeNull();
    doc.change(MIXED);
    expect(session.getModel().readOnly).toBe(MIXED);
  });
});
