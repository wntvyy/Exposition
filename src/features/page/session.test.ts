import { describe, expect, it } from "vitest";

import { imageType } from "./session";

describe("page images", () => {
  it("detects the media type of page images by content", () => {
    expect(imageType(new TextEncoder().encode('<svg viewBox="0 0 1 1"></svg>'))).toBe("image/svg+xml");
    expect(imageType(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a]))).toBe("image/png");
  });
});
