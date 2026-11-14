// The P1 scenarios need Windows facilities (Recycle Bin, CF_HDROP clipboard,
// junctions, sharing violations, read back through PowerShell). Elsewhere they
// skip with a clear reason instead of failing.
import { describe, expect, it } from "vitest";

import { platformSkip } from "./p1-helpers.mjs";

describe("platformSkip", () => {
  it("runs on Windows", () => {
    expect(platformSkip("win32")).toBeNull();
  });

  it("skips elsewhere and says why", () => {
    for (const platform of ["linux", "darwin"]) {
      const reason = platformSkip(platform);
      expect(reason).toMatch(/Windows/);
      expect(reason).toMatch(/Recycle Bin|clipboard/);
    }
  });
});
