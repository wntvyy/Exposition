import { describe, expect, it } from "vitest";

import { RevisionHistory, mapBackward, mapForward } from "./mapping";

const insert = (at: number, text: string) => ({ range: { start: at, end: at }, text });
const del = (start: number, end: number) => ({ range: { start, end }, text: "" });

describe("offset mapping", () => {
  it("maps forward through insertions and deletions (same as the Rust mapping)", () => {
    const p = [insert(2, "xy"), del(5, 7)];
    expect(mapForward(1, p, "downstream")).toBe(1);
    expect(mapForward(2, p, "downstream")).toBe(4);
    expect(mapForward(2, p, "upstream")).toBe(2);
    expect(mapForward(4, p, "downstream")).toBe(6);
    expect(mapForward(6, p, "upstream")).toBe(7);
    expect(mapForward(9, p, "downstream")).toBe(9);
  });

  it("maps backward to the old revision", () => {
    const p = [insert(2, "xy"), del(5, 7)];
    expect(mapBackward(1, p)).toBe(1);
    expect(mapBackward(3, p)).toBe(2);
    expect(mapBackward(4, p)).toBe(2);
    expect(mapBackward(6, p)).toBe(4);
    expect(mapBackward(7, p)).toBe(7);
  });

  it("walks a revision history in both directions", () => {
    const h = new RevisionHistory();
    h.add({ previous: 1, revision: 2, patches: [insert(0, "ab")] });
    h.add({ previous: 2, revision: 3, patches: [insert(10, "c")] });
    expect(h.forward(5, 1, 3, "downstream")).toBe(7);
    expect(h.backward(7, 3, 1)).toBe(5);
    expect(h.forward(5, 1, 4, "downstream")).toBeNull();
    h.prune(2);
    expect(h.backward(7, 3, 1)).toBeNull();
  });

  it("keeps astral characters intact (UTF-16 units)", () => {
    const text = "a😀b";
    expect(text.length).toBe(4);
    expect(mapForward(3, [insert(1, "😀")], "downstream")).toBe(5);
  });
});
