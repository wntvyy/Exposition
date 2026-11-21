// Review: the memo of build markers must never leave a document with markers
// that the unmemoized computation would have replaced. Drives `markers.ts`
// with the real store and a fake Monaco that records every marker write.
import { beforeEach, describe, expect, it, vi } from "vitest";

const fake = vi.hoisted(() => {
  interface Model {
    name: string;
  }
  interface Handle {
    closed: boolean;
    path: string | null;
    model: Model;
    sync: { revision: number; pendingCount: number };
  }
  return {
    handles: [] as Handle[],
    /** Marker messages per model after the last write. */
    markers: new Map<Model, string[]>(),
    writes: 0,
    created: [] as Array<(model: Model) => void>,
  };
});

vi.mock("../../lib/monaco", () => ({
  monaco: {
    MarkerSeverity: { Error: 8, Warning: 4, Info: 2 },
    editor: {
      setModelMarkers: (model: { name: string }, _owner: string, markers: Array<{ message: string }>) => {
        fake.writes += 1;
        fake.markers.set(
          model,
          markers.map((m) => m.message),
        );
      },
      onDidCreateModel: (listener: (model: { name: string }) => void) => {
        fake.created.push(listener);
      },
    },
  },
}));
vi.mock("../editor", () => ({ openHandles: () => fake.handles }));
vi.mock("../../lib/ipc", () => ({ events: {} }));

import type { BuildEvent } from "../../bindings/BuildEvent";
import { installBuildMarkers } from "./markers";
import { clearBuildStates, recordActivity, recordBuild } from "./store";

let sequence = 0;
function build(entry: string, sources: Array<[string, number]>, errors: Array<[string, string]>): BuildEvent {
  return {
    result: {
      id: `typst-${++sequence}`,
      target: { format: "typst", engine: "typst", entry },
      snapshot: "s",
      sources: sources.map(([path, revision]) => ({ path, document: "d", revision, contentHash: "h" })),
      configuration: null,
      status: { kind: errors.length ? "failed" : "succeeded" },
      diagnostics: errors.map(([path, message]) => ({
        severity: "error",
        message,
        location: { path, range: { start: 0, end: 1 } },
        hints: [],
      })),
      pages: [],
      mapping: "none",
      exports: [],
      durationMs: 1,
    },
    timings: { snapshotMs: 0, compileMs: 0, mapMs: 0, totalMs: 0 },
    lastGood: null,
    refusal: null,
  } as unknown as BuildEvent;
}

function handle(path: string, revision: number) {
  const model = { name: path, getPositionAt: (offset: number) => ({ lineNumber: 1, column: offset + 1 }) };
  const created = { closed: false, path: path as string | null, model, sync: { revision, pendingCount: 0 } };
  fake.handles.push(created);
  return created;
}

const shown = (h: { model: { name: string } }) => fake.markers.get(h.model);

describe("review: build markers with the memo", () => {
  beforeEach(() => {
    installBuildMarkers();
    clearBuildStates();
    fake.handles.length = 0;
    fake.markers.clear();
    fake.writes = 0;
  });

  it("a failure, pending edits, the retry after them, and the fix", () => {
    const doc = handle("main.typ", 1);
    recordBuild(build("main.typ", [["main.typ", 1]], [["main.typ", "first error"]]));
    expect(shown(doc)).toEqual(["first error"]);
    // Activity only: nothing is written again.
    const writes = fake.writes;
    recordActivity("main.typ", "queued");
    recordActivity("main.typ", "building");
    recordActivity("main.typ", "idle");
    expect(fake.writes).toBe(writes);
    // The person types: the edit is pending when the build of it arrives.
    doc.sync.pendingCount = 1;
    recordBuild(build("main.typ", [["main.typ", 2]], []));
    expect(shown(doc)).toEqual(["first error"]); // positions belong to revision 1 still
    // The edit is acknowledged; the next notification is activity only.
    doc.sync.pendingCount = 0;
    doc.sync.revision = 2;
    recordActivity("main.typ", "idle");
    expect(shown(doc), "the fixed build's (empty) markers after the pending edit").toEqual([]);
    // A failure again, then success for the same revision (an included file was fixed).
    recordBuild(build("main.typ", [["main.typ", 2]], [["main.typ", "second error"]]));
    expect(shown(doc)).toEqual(["second error"]);
    recordBuild(build("main.typ", [["main.typ", 2]], []));
    expect(shown(doc)).toEqual([]);
  });

  it("a pending edit that was skipped is not remembered as applied", () => {
    const doc = handle("main.typ", 1);
    doc.sync.pendingCount = 1;
    const event = build("main.typ", [["main.typ", 1]], [["main.typ", "error"]]);
    recordBuild(event);
    expect(shown(doc)).toBeUndefined();
    // The edit is rejected (the revision stays): the same build still applies.
    doc.sync.pendingCount = 0;
    recordActivity("main.typ", "idle");
    expect(shown(doc)).toEqual(["error"]);
  });

  it("a renamed document, a recreated model, and two entries that compile one file", () => {
    const doc = handle("chapter.typ", 3);
    recordBuild(build("main.typ", [["main.typ", 1], ["chapter.typ", 3]], [["chapter.typ", "from main"]]));
    recordBuild(build("other.typ", [["other.typ", 1], ["chapter.typ", 3]], [["chapter.typ", "from other"]]));
    expect(shown(doc)).toEqual(["from main", "from other"]);
    // Renamed: no retained build compiled that path, the markers stay as they are.
    doc.path = "renamed.typ";
    recordActivity("main.typ", "idle");
    expect(shown(doc)).toEqual(["from main", "from other"]);
    // A build that compiled it under the new name replaces them.
    recordBuild(build("main.typ", [["main.typ", 1], ["renamed.typ", 3]], [["renamed.typ", "after rename"]]));
    expect(shown(doc)).toEqual(["after rename"]);
    // Closed and opened again: a new model gets the markers although no build arrives.
    fake.handles.length = 0;
    const reopened = handle("renamed.typ", 3);
    expect(shown(reopened)).toBeUndefined();
    fake.created.forEach((listener) => listener(reopened.model));
    return Promise.resolve().then(() => {
      expect(shown(reopened)).toEqual(["after rename"]);
      // One entry is fixed, the other still reports.
      recordBuild(build("main.typ", [["main.typ", 2], ["renamed.typ", 3]], []));
      expect(shown(reopened)).toEqual([]);
    });
  });
});
