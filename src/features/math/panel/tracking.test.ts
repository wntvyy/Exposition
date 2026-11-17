// Target tracking: a draft's range follows changes before it, ignores changes
// after it, and becomes conflicted (never silently re-mapped) by any change
// touching it, a flush, or a model that went away.
import { describe, expect, it } from "vitest";

import { TrackedRange, type ContentChange } from "./tracking";
import { FakeModel } from "./testing/fakeDocument";

const change = (rangeOffset: number, rangeLength: number, text: string): ContentChange => ({ rangeOffset, rangeLength, text });

describe("TrackedRange", () => {
  it("shifts with edits before the range and ignores edits after it", () => {
    const model = new FakeModel("ab $x+1$ cd");
    const range = new TrackedRange(model, 3, 8, "$x+1$");
    model.edit([change(0, 0, "ZZ")]);
    expect([range.start, range.end]).toEqual([5, 10]);
    model.edit([change(11, 2, "")]);
    expect([range.start, range.end]).toEqual([5, 10]);
    expect(range.verify()).toBeNull();
  });

  it("treats insertions exactly at the edges as outside a non-empty range", () => {
    const model = new FakeModel("$x$");
    const range = new TrackedRange(model, 0, 3, "$x$");
    model.edit([change(0, 0, "A ")]);
    model.edit([change(5, 0, " B")]);
    expect([range.start, range.end]).toEqual([2, 5]);
    expect(range.verify()).toBeNull();
  });

  it("maps several changes of one event relative to the text before it", () => {
    const model = new FakeModel("0123456789");
    const range = new TrackedRange(model, 4, 6, "45");
    model.edit([change(8, 1, "XYZ"), change(1, 2, "")]);
    expect([range.start, range.end]).toEqual([2, 4]);
    expect(range.verify()).toBeNull();
  });

  it("becomes conflicted by a change inside the range and reports it once", () => {
    const model = new FakeModel("ab $x+1$ cd");
    const range = new TrackedRange(model, 3, 8, "$x+1$");
    const seen: string[] = [];
    range.onProblem = (p) => seen.push(p.kind);
    model.edit([change(5, 1, "-")]);
    model.edit([change(4, 0, "y")]);
    expect(range.conflict).toEqual({ kind: "changed" });
    expect(seen).toEqual(["changed"]);
  });

  it("treats any change at an empty insertion point as a conflict", () => {
    const model = new FakeModel("hello world");
    const point = new TrackedRange(model, 5, 5, "");
    model.edit([change(5, 0, "!")]);
    expect(point.conflict?.kind).toBe("changed");
  });

  it("detects a flush (model text replaced) and a disposed model", () => {
    const model = new FakeModel("$x$");
    const range = new TrackedRange(model, 0, 3, "$x$");
    model.flush("$y$");
    expect(range.conflict?.kind).toBe("replaced");
    const other = new FakeModel("$x$");
    const closed = new TrackedRange(other, 0, 3, "$x$");
    other.dispose();
    expect(closed.conflict?.kind).toBe("closed");
  });

  it("verify catches a text difference the events did not reveal", () => {
    const model = new FakeModel("$x$");
    const range = new TrackedRange(model, 0, 3, "$x$");
    expect(range.verify("$y$")).toEqual({ kind: "changed" });
  });

  it("stops listening when disposed", () => {
    const model = new FakeModel("$x$ text");
    const range = new TrackedRange(model, 0, 3, "$x$");
    range.dispose();
    model.edit([change(1, 1, "y")]);
    expect(range.conflict).toBeNull();
    expect(model.listenerCount).toBe(0);
  });
});
