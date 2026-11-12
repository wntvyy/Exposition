import { describe, expect, it } from "vitest";

import { buildSequence, isOlder } from "./buildOrder";

describe("build ordering (R-m23 F4)", () => {
  it("orders builds by compiled revision, then by sequence", () => {
    expect(buildSequence("typst-12")).toBe(12);
    expect(buildSequence("tex-3")).toBe(3);
    expect(buildSequence("odd")).toBe(0);
    // A build of the same revision made later is newer (typst-9 vs typst-1).
    expect(isOlder({ revision: 4, seq: 1 }, { revision: 4, seq: 9 })).toBe(true);
    expect(isOlder({ revision: 4, seq: 9 }, { revision: 4, seq: 1 })).toBe(false);
    expect(isOlder({ revision: 5, seq: 1 }, { revision: 4, seq: 9 })).toBe(false);
    expect(isOlder({ revision: 3, seq: 20 }, { revision: 4, seq: 9 })).toBe(true);
  });
});
