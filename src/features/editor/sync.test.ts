import { describe, expect, it } from "vitest";

import type { DocumentChanged } from "../../bindings/DocumentChanged";
import type { DocumentState } from "../../bindings/DocumentState";
import type { TextPatch } from "../../bindings/TextPatch";
import type { TransactionOutcome } from "../../bindings/TransactionOutcome";
import type { TransactionRequest } from "../../bindings/TransactionRequest";
import { applyPatches, diffPatch } from "./ot";
import { DocumentSync, type SyncTransport, type SyncView } from "./sync";
import { UndoGrouper } from "./typing";

// --- A faithful in-memory model of the document service's rebase semantics ---
// (independent port of `map_range`/`rebase` in crates/exposition-workspace/src/documents.rs).

function serviceMapRange(range: { start: number; end: number }, concurrent: TextPatch[]) {
  let shift = 0;
  for (const q of concurrent) {
    const delta = q.text.length - (q.range.end - q.range.start);
    if (q.range.start === q.range.end) {
      if (q.range.start <= range.start) shift += delta;
      else if (q.range.start < range.end) return null;
    } else if (q.range.end <= range.start) shift += delta;
    else if (q.range.start >= range.end) {
      /* after */
    } else return null;
  }
  return { start: range.start + shift, end: range.end + shift };
}

class FakeService {
  text: string;
  revision = 1;
  history: { revision: number; patches: TextPatch[]; inverse: TextPatch[] }[] = [];
  /** Requests waiting to be processed (simulates IPC latency). */
  inbox: (() => void)[] = [];
  /** Events waiting to be delivered to the client. */
  outbox: DocumentChanged[] = [];
  received: TransactionRequest[] = [];

  constructor(text: string) {
    this.text = text;
  }

  commit(patches: TextPatch[], transaction: string | null, kind: DocumentChanged["kind"], origin: DocumentChanged["origin"]) {
    const before = this.text;
    this.text = applyPatches(before, patches);
    // inverse relative to new text
    let shift = 0;
    const inverse = patches.map((p) => {
      const start = p.range.start + shift;
      shift += p.text.length - (p.range.end - p.range.start);
      return { range: { start, end: start + p.text.length }, text: before.slice(p.range.start, p.range.end) };
    });
    const previousRevision = this.revision;
    this.revision += 1;
    this.history.push({ revision: this.revision, patches, inverse });
    this.outbox.push({
      document: "doc-1",
      transaction,
      origin,
      kind,
      previousRevision,
      revision: this.revision,
      patches,
      selection: null,
      canUndo: true,
      canRedo: false,
    });
  }

  applyNow(request: TransactionRequest): TransactionOutcome {
    const edit = request.edits[0]!;
    let patches = edit.patches;
    if (edit.baseRevision !== this.revision) {
      if (request.conflict === "reject") throw { kind: "staleRevision" };
      for (const entry of this.history.filter((h) => h.revision > edit.baseRevision)) {
        const mapped: TextPatch[] = [];
        for (const p of patches) {
          const range = serviceMapRange(p.range, entry.patches);
          if (!range) throw { kind: "conflict" };
          mapped.push({ range, text: p.text });
        }
        patches = mapped;
      }
    }
    const previousRevision = this.revision;
    this.commit(patches, request.id, "edit", request.origin);
    return {
      id: request.id,
      applied: [{ document: "doc-1", previousRevision, revision: this.revision, patches, rebased: false }],
    };
  }

  /** Another origin (agent, page editor) edits at the current revision. */
  remote(patches: TextPatch[]) {
    this.commit(patches, null, "edit", { kind: "agent", taskId: "t" });
  }

  undoNow() {
    const last = this.history.at(-1);
    if (!last) throw { kind: "nothingToUndo" };
    this.commit(last.inverse, null, "undo", { kind: "source" });
  }

  transport(): SyncTransport {
    return {
      apply: (request) =>
        new Promise((resolve, reject) => {
          this.received.push(request);
          this.inbox.push(() => {
            try {
              resolve(this.applyNow(request));
            } catch (e) {
              reject(e);
            }
          });
        }),
      undo: () =>
        new Promise((resolve, reject) =>
          this.inbox.push(() => {
            try {
              this.undoNow();
              resolve(null);
            } catch (e) {
              reject(e);
            }
          }),
        ),
      redo: () => Promise.reject({ kind: "nothingToUndo" }),
      state: async (): Promise<DocumentState> => ({
        text: this.text,
        info: {
          id: "doc-1",
          path: "main.typ",
          revision: this.revision,
          savedRevision: 1,
          language: "typst",
          lineEnding: "lf",
          encoding: "utf8",
          readOnly: null,
          canUndo: false,
          canRedo: false,
        },
      }),
    };
  }

  /** Process one queued request. */
  processOne() {
    this.inbox.shift()?.();
  }
}

class FakeView implements SyncView {
  constructor(public text: string) {}
  applyPatches(patches: TextPatch[]) {
    this.text = applyPatches(this.text, patches);
  }
  canApply() {
    return true;
  }
  setText(text: string) {
    this.text = text;
  }
}

/** Let pending promise callbacks run (the sync logic only uses promise chains). */
const tick = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve();
};

function setup(text: string) {
  const service = new FakeService(text);
  const view = new FakeView(text);
  const rejected: unknown[] = [];
  const sync = new DocumentSync("doc-1", 1, service.transport(), view, { onRejected: (e) => rejected.push(e) });
  /** Simulate the user typing: edit the view, then report it. */
  const type = (patch: TextPatch) => {
    view.text = applyPatches(view.text, [patch]);
    sync.local([patch], { kind: "newGroup" });
  };
  const deliver = () => {
    const events = service.outbox;
    service.outbox = [];
    for (const e of events) sync.receive(e);
  };
  /** Run everything to quiescence. */
  const settle = async () => {
    for (let i = 0; i < 100; i++) {
      await tick();
      if (!service.inbox.length && !service.outbox.length && sync.pendingCount === 0 && !sync.isResyncing) return;
      service.processOne();
      await tick();
      deliver();
    }
    throw new Error("did not settle");
  };
  return { service, view, sync, type, deliver, settle, rejected };
}

const ins = (at: number, text: string): TextPatch => ({ range: { start: at, end: at }, text });
const rep = (start: number, end: number, text: string): TextPatch => ({ range: { start, end }, text });

describe("DocumentSync", () => {
  it("sends local edits one at a time, each based on the latest acknowledged revision", async () => {
    const { service, view, sync, type, deliver, settle } = setup("hello");
    type(ins(5, " "));
    type(ins(6, "w"));
    type(ins(7, "o"));
    expect(service.received).toHaveLength(1);
    expect(service.received[0]!.edits[0]!.baseRevision).toBe(1);
    service.processOne();
    await tick();
    deliver();
    expect(service.received).toHaveLength(2);
    expect(service.received[1]!.edits[0]!.baseRevision).toBe(2);
    await settle();
    expect(service.text).toBe("hello wo");
    expect(view.text).toBe("hello wo");
    expect(sync.revision).toBe(4);
    for (const r of service.received) {
      expect(r.conflict).toBe("rebaseDisjoint");
      expect(r.origin).toEqual({ kind: "source" });
    }
  });

  it("handles the acknowledgement event arriving before or after the command response", async () => {
    const { service, view, sync, type, deliver, settle } = setup("ab");
    type(ins(1, "x"));
    // Event first, response later.
    service.processOne();
    deliver();
    type(ins(2, "y"));
    await settle();
    expect(view.text).toBe("axyb");
    expect(service.text).toBe("axyb");
    expect(sync.pendingCount).toBe(0);
  });

  it("transforms a remote revision over an in-flight local edit (remote applied first)", async () => {
    const { service, view, type, deliver, settle } = setup("The cat sat.");
    type(ins(8, "quietly ")); // in flight, based on revision 1
    service.remote([ins(4, "big ")]); // an agent edit lands first
    deliver(); // the view sees the agent edit while its own edit is pending
    expect(view.text).toBe("The big cat quietly sat.");
    service.processOne(); // the service rebases the local edit over the agent edit
    await tick();
    deliver();
    await settle();
    expect(service.text).toBe("The big cat quietly sat.");
    expect(view.text).toBe(service.text);
  });

  it("transforms remote revisions over unsent buffered edits too", async () => {
    const { service, view, type, deliver, settle } = setup("0123456789");
    type(ins(10, "A")); // in flight
    type(ins(11, "B")); // buffered
    type(rep(0, 1, "z")); // buffered, at the start
    service.remote([rep(4, 6, "--")]);
    deliver();
    expect(view.text).toBe("z123--6789AB");
    await settle();
    expect(service.text).toBe("z123--6789AB");
    expect(view.text).toBe(service.text);
  });

  it("orders concurrent insertions at the same point like the service", async () => {
    const { service, view, type, deliver, settle } = setup("abc");
    type(ins(3, "L"));
    service.remote([ins(3, "R")]);
    deliver();
    await settle();
    expect(service.text).toBe("abcRL");
    expect(view.text).toBe("abcRL");
  });

  it("resyncs from the service when a local edit is rejected", async () => {
    const { service, view, sync, type, deliver, settle, rejected } = setup("hello world");
    type(rep(0, 5, "HELLO")); // in flight
    service.remote([rep(2, 4, "ZZ")]); // overlaps; applied first on the service
    // The view cannot transform the overlap, so it resyncs immediately.
    deliver();
    await settle();
    expect(view.text).toBe(service.text);
    expect(service.text).toBe("heZZo world");
    expect(sync.pendingCount).toBe(0);
    // The stale local edit reached the service and was refused, not applied.
    expect(service.received.length).toBe(1);
    void rejected;
  });

  it("resyncs after a rejected transaction and keeps working", async () => {
    const { service, view, sync, type, settle, rejected } = setup("abc");
    service.applyNow = () => {
      throw { kind: "readOnly", reason: "test" };
    };
    type(ins(0, "x"));
    await settle();
    expect(rejected).toEqual([{ kind: "readOnly", reason: "test" }]);
    expect(view.text).toBe("abc");
    expect(sync.pendingCount).toBe(0);
  });

  it("resyncs on a revision gap", async () => {
    const { service, view, sync } = setup("abc");
    service.remote([ins(0, "1")]);
    service.remote([ins(0, "2")]);
    sync.receive(service.outbox[1]!); // the first event was missed
    await tick();
    await tick();
    expect(view.text).toBe("21abc");
    expect(sync.revision).toBe(3);
    sync.receive(service.outbox[0]!); // late duplicate is ignored
    expect(view.text).toBe("21abc");
  });

  it("routes undo through the queue after pending edits and applies the result", async () => {
    const { service, view, sync, type, settle } = setup("ab");
    type(ins(2, "c"));
    sync.history("undo");
    type(ins(3, "d")); // typed while the undo is queued
    await settle();
    expect(service.text).toBe(view.text);
    expect(view.text).toBe("abd");
  });

  it("idle() resolves after every pending edit is acknowledged", async () => {
    const { service, sync, type, settle } = setup("");
    type(ins(0, "a"));
    let idle = false;
    void sync.idle().then(() => (idle = true));
    await tick();
    expect(idle).toBe(false);
    await settle();
    await tick();
    expect(idle).toBe(true);
    expect(service.text).toBe("a");
  });

  it("converges under random interleavings of local and remote edits", async () => {
    let seed = 12345;
    const random = (n: number) => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed % n;
    };
    for (let round = 0; round < 200; round++) {
      const { service, view, sync, type, deliver, settle } = setup("the quick brown fox");
      for (let step = 0; step < 30; step++) {
        const action = random(5);
        if (action === 0 || action === 1) {
          const at = random(view.text.length + 1);
          if (random(3) === 0 && at < view.text.length) type(rep(at, Math.min(view.text.length, at + 1 + random(3)), ""));
          else type(ins(at, "xyz"[random(3)]!));
        } else if (action === 2) {
          const at = random(service.text.length + 1);
          service.remote([ins(at, "R")]);
        } else if (action === 3) {
          service.processOne();
          await tick();
        } else {
          deliver();
        }
      }
      await settle();
      expect(view.text, `round ${round}`).toBe(service.text);
      expect(sync.revision).toBe(service.revision);
    }
  });
});

describe("UndoGrouper", () => {
  it("merges contiguous typing and rolls over on pauses, jumps, kind switches, and pastes", () => {
    const g = new UndoGrouper();
    const a = g.classify([ins(0, "a")], 0);
    const b = g.classify([ins(1, "b")], 100);
    expect(a).toEqual(b);
    expect(a.kind).toBe("merge");
    const pause = g.classify([ins(2, "c")], 100 + 1500);
    expect(pause).not.toEqual(b);
    const jump = g.classify([ins(10, "d")], 1700);
    expect(jump).not.toEqual(pause);
    const del = g.classify([rep(10, 11, "")], 1750);
    expect(del).not.toEqual(jump);
    const del2 = g.classify([rep(9, 10, "")], 1800);
    expect(del2).toEqual(del);
    expect(g.classify([ins(9, "pasted text")], 1850)).toEqual({ kind: "newGroup" });
    expect(g.classify([ins(0, "a"), ins(5, "a")], 1900)).toEqual({ kind: "newGroup" });
    const afterPaste = g.classify([ins(20, "e")], 1950);
    expect(afterPaste.kind).toBe("merge");
    const enter = g.classify([ins(21, "\n  ")], 2000);
    expect(enter.kind).toBe("merge");
    expect(enter).not.toEqual(afterPaste);
  });

  it("keeps an IME composition in one group", () => {
    const g = new UndoGrouper();
    g.setComposing(true);
    const k1 = g.classify([ins(0, "k")], 0);
    const k2 = g.classify([rep(0, 1, "か")], 50);
    const k3 = g.classify([rep(0, 1, "漢字")], 3000);
    g.setComposing(false);
    expect(k1).toEqual(k2);
    expect(k2).toEqual(k3);
  });
});

describe("diffPatch", () => {
  it("produces one minimal patch without splitting surrogate pairs", () => {
    expect(diffPatch("abc", "abc")).toBeNull();
    expect(diffPatch("hello world", "hello brave world")).toEqual(ins(6, "brave "));
    const from = "a😀b";
    const to = "a😁b";
    const patch = diffPatch(from, to)!;
    expect(applyPatches(from, [patch])).toBe(to);
    expect(patch.range.start).toBe(1);
    expect(patch.range.end).toBe(3);
  });
});
