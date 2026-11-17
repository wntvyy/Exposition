import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({
  mediaReadImage: vi.fn<(path: string) => Promise<ArrayBuffer>>(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: { mediaReadImage: ipc.mediaReadImage },
  events: { workspace: () => Promise.resolve(() => {}) },
  isCommandError: (v: unknown) => typeof v === "object" && v !== null && "code" in v,
}));

import { fitZoom, ImageViewer, stepZoom } from "./ImageViewer";
import { handleWorkspaceEvent, resetImageCacheForTests, setImageDecoderForTests } from "./loader";

function png(width: number, height: number): ArrayBuffer {
  const b = new Uint8Array(33);
  b.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52]);
  new DataView(b.buffer).setUint32(16, width);
  new DataView(b.buffer).setUint32(20, height);
  return b.buffer;
}

const HOSTILE_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="40" height="20" onload="window.__svgOnload=1">' +
  '<script>window.__svgScript = 1</script><image href="https://example.invalid/track.png" width="10" height="10"/>' +
  '<foreignObject><iframe xmlns="http://www.w3.org/1999/xhtml" src="https://example.invalid/"></iframe></foreignObject></svg>';

const resource = (path: string) => ({ key: `file:${path}`, path, title: path.split("/").pop()! });

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    disconnect() {}
    unobserve() {}
  } as never;
});

beforeEach(() => {
  resetImageCacheForTests();
  let n = 0;
  URL.createObjectURL = vi.fn((blob: Blob) => `blob:v-${++n}-${blob.type}`);
  URL.revokeObjectURL = vi.fn();
  setImageDecoderForTests(async (_url, format) => (format === "svg" ? { width: 40, height: 20, preview: null } : { width: 800, height: 600, preview: null }));
  ipc.mediaReadImage.mockReset();
});
afterEach(() => {
  cleanup();
  setImageDecoderForTests(null);
});

describe("zoom helpers", () => {
  it("steps through fixed zoom levels within 10%–800%", () => {
    expect(stepZoom(1, 1)).toBe(1.25);
    expect(stepZoom(1, -1)).toBe(0.75);
    expect(stepZoom(0.8, 1)).toBe(1);
    expect(stepZoom(8, 1)).toBe(8);
    expect(stepZoom(0.1, -1)).toBe(0.1);
  });
  it("fits without enlarging", () => {
    expect(fitZoom(800, 600, 400, 600)).toBe(0.5);
    expect(fitZoom(100, 100, 400, 400)).toBe(1);
  });
});

describe("ImageViewer", () => {
  it("shows a PNG with its facts and zooms with the toolbar", async () => {
    ipc.mediaReadImage.mockResolvedValue(png(800, 600));
    render(<ImageViewer resource={resource("figs/plot one.png")} mode="source" active />);
    const img = await screen.findByTestId("image-viewer-image");
    expect(img.getAttribute("src")).toMatch(/^blob:v-\d+-image\/png$/);
    expect(img.getAttribute("alt")).toBe("plot one.png");
    expect(screen.getByTestId("image-facts").textContent).toBe("800 × 600 · PNG · 33 B");
    const viewer = screen.getByTestId("image-viewer");
    expect(viewer.getAttribute("data-fit")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Actual size" }));
    expect(viewer.getAttribute("data-zoom")).toBe("1");
    expect(viewer.getAttribute("data-fit")).toBe("false");
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    expect(viewer.getAttribute("data-zoom")).toBe("1.25");
    expect(img.getAttribute("width")).toBe("1000");
    fireEvent.keyDown(screen.getByRole("group"), { key: "-" });
    expect(viewer.getAttribute("data-zoom")).toBe("1");
    fireEvent.click(screen.getByRole("button", { name: "Fit" }));
    expect(viewer.getAttribute("data-fit")).toBe("true");
    expect(ipc.mediaReadImage).toHaveBeenCalledTimes(1);
  });

  it("shows SVG only as an <img> from a blob URL: no markup, scripts, or external loads in the DOM", async () => {
    ipc.mediaReadImage.mockResolvedValue(new TextEncoder().encode(HOSTILE_SVG).buffer as ArrayBuffer);
    const { container } = render(<ImageViewer resource={resource("evil.svg")} mode="source" active />);
    const img = await screen.findByTestId("image-viewer-image");
    expect(img.tagName).toBe("IMG");
    expect(img.getAttribute("src")).toMatch(/^blob:v-\d+-image\/svg\+xml$/);
    expect(container.querySelector("script, iframe, object, embed, foreignObject, image")).toBeNull();
    expect(container.innerHTML).not.toContain("example.invalid");
    expect(container.innerHTML).not.toContain("__svg");
    expect((window as unknown as Record<string, unknown>).__svgScript).toBeUndefined();
    expect((window as unknown as Record<string, unknown>).__svgOnload).toBeUndefined();
  });

  it("names missing, unsupported, damaged, and oversized files without an editable buffer", async () => {
    const cases: [string, () => Promise<ArrayBuffer>, string, string][] = [
      ["gone.png", () => Promise.reject({ code: "path.notFound", message: "gone.png does not exist." }), "missing", "Image not found"],
      ["scan.tiff", () => Promise.reject({ code: "image.unsupported", message: "x" }), "unsupported", "Can't show this image format"],
      ["fake.png", async () => new TextEncoder().encode("hello").buffer as ArrayBuffer, "unsupported", "Can't show this image format"],
      ["cut.png", async () => png(1, 1).slice(0, 16), "damaged", "Can't show this image"],
      ["huge.png", async () => png(30000, 30000), "tooLarge", "Image too large to show"],
      ["link.png", () => Promise.reject({ code: "path.outsideWorkspace", message: "link.png resolves outside the open folder." }), "blocked", "Image outside the project"],
    ];
    for (const [path, read, problem, title] of cases) {
      ipc.mediaReadImage.mockImplementationOnce(read);
      const { unmount } = render(<ImageViewer resource={resource(path)} mode="source" active />);
      const state = await screen.findByTestId("image-viewer-state");
      expect(state.getAttribute("data-problem"), path).toBe(problem);
      expect(state.textContent).toContain(title);
      expect(document.querySelector("textarea, [contenteditable=true], .monaco-editor")).toBeNull();
      unmount();
    }
  });

  it("reloads when the file is replaced and reports when it is removed", async () => {
    ipc.mediaReadImage.mockResolvedValueOnce(png(800, 600));
    render(<ImageViewer resource={resource("a.png")} mode="source" active />);
    const first = (await screen.findByTestId("image-viewer-image")).getAttribute("src");
    ipc.mediaReadImage.mockResolvedValueOnce(png(800, 600));
    await act(async () => handleWorkspaceEvent({ kind: "filesChanged", changes: [{ path: "a.png", kind: "modified" }], rescan: false }));
    await waitFor(() => expect(screen.getByTestId("image-viewer-image").getAttribute("src")).not.toBe(first));
    ipc.mediaReadImage.mockRejectedValueOnce({ code: "path.notFound", message: "a.png does not exist." });
    await act(async () => handleWorkspaceEvent({ kind: "filesChanged", changes: [{ path: "a.png", kind: "removed" }], rescan: false }));
    expect((await screen.findByTestId("image-viewer-state")).getAttribute("data-problem")).toBe("missing");
  });
});
