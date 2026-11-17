import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({
  mediaReadImage: vi.fn<(path: string) => Promise<ArrayBuffer>>(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: { mediaReadImage: ipc.mediaReadImage },
  events: { workspace: () => Promise.resolve(() => {}) },
  isCommandError: (v: unknown) => typeof v === "object" && v !== null && "code" in v && "message" in v,
}));

import {
  acquireImage,
  handleWorkspaceEvent,
  ImageLoadError,
  MAX_IMAGE_PIXELS,
  onImageChange,
  rasterSize,
  readProjectImage,
  resetImageCacheForTests,
  setImageDecoderForTests,
  sizeProblem,
  sniffImage,
} from "./loader";

// ----- fixtures ------------------------------------------------------------------------------

function png(width: number, height: number): Uint8Array {
  const b = new Uint8Array(33);
  b.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52]);
  new DataView(b.buffer).setUint32(16, width);
  new DataView(b.buffer).setUint32(20, height);
  return b;
}

function jpeg(width: number, height: number): Uint8Array {
  // SOI, APP0 (length 16), SOF0 with height/width.
  const app0 = [0xff, 0xe0, 0x00, 0x10, ...new Array(14).fill(0)];
  const sof = [0xff, 0xc0, 0x00, 0x11, 0x08, height >> 8, height & 255, width >> 8, width & 255, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0];
  return new Uint8Array([0xff, 0xd8, ...app0, ...sof, 0xff, 0xd9]);
}

const gif = (w: number, h: number) => new Uint8Array([0x47, 0x49, 0x46, 0x38, 0x39, 0x61, w & 255, w >> 8, h & 255, h >> 8, 0, 0, 0]);

function webp(kind: "VP8 " | "VP8L" | "VP8X", w: number, h: number): Uint8Array {
  const b = new Uint8Array(40);
  const text = (at: number, s: string) => s.split("").forEach((c, i) => (b[at + i] = c.charCodeAt(0)));
  text(0, "RIFF");
  text(8, "WEBP");
  text(12, kind);
  if (kind === "VP8 ") {
    b[26] = w & 255;
    b[27] = w >> 8;
    b[28] = h & 255;
    b[29] = h >> 8;
  } else if (kind === "VP8L") {
    const W = w - 1;
    const H = h - 1;
    b[21] = W & 255;
    b[22] = ((W >> 8) & 0x3f) | ((H & 3) << 6);
    b[23] = (H >> 2) & 255;
    b[24] = (H >> 10) & 0x0f;
  } else {
    const W = w - 1;
    const H = h - 1;
    b.set([W & 255, (W >> 8) & 255, (W >> 16) & 255, H & 255, (H >> 8) & 255, (H >> 16) & 255], 24);
  }
  return b;
}

function bmp(w: number, h: number): Uint8Array {
  const b = new Uint8Array(54);
  b[0] = 0x42;
  b[1] = 0x4d;
  b[14] = 40;
  const v = new DataView(b.buffer);
  v.setInt32(18, w, true);
  v.setInt32(22, -h, true); // top-down
  return b;
}

const svg = (body: string) => new TextEncoder().encode(body);
const buffer = (b: Uint8Array) => b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength) as ArrayBuffer;

// ----- format and size --------------------------------------------------------------------------

describe("format detection from bytes", () => {
  it("recognizes the displayed formats by content, not extension", () => {
    expect(sniffImage(png(2, 3))).toBe("png");
    expect(sniffImage(jpeg(2, 3))).toBe("jpeg");
    expect(sniffImage(gif(2, 3))).toBe("gif");
    expect(sniffImage(webp("VP8X", 2, 3))).toBe("webp");
    expect(sniffImage(bmp(2, 3))).toBe("bmp");
    expect(sniffImage(svg('<?xml version="1.0"?>\n<!-- c -->\n<svg xmlns="http://www.w3.org/2000/svg"/>'))).toBe("svg");
    expect(sniffImage(svg("﻿  <svg>"))).toBe("svg");
  });

  it("rejects other content", () => {
    expect(sniffImage(new Uint8Array())).toBeNull();
    expect(sniffImage(svg("TOKEN=secret"))).toBeNull();
    expect(sniffImage(svg("<html><body>not svg</body></html>"))).toBeNull();
    expect(sniffImage(new Uint8Array([0x49, 0x49, 0x2a, 0x00, 1, 2, 3, 4]))).toBeNull(); // TIFF
    expect(sniffImage(svg("%PDF-1.7 <svg>"))).toBeNull();
    expect(sniffImage(new Uint8Array([0x3c, 0x73, 0x76, 0x67, 0, 0x3e]))).toBeNull(); // NUL: binary
  });

  it("reads raster sizes from headers without decoding", () => {
    expect(rasterSize(png(640, 480), "png")).toEqual({ width: 640, height: 480 });
    expect(rasterSize(jpeg(1920, 1080), "jpeg")).toEqual({ width: 1920, height: 1080 });
    expect(rasterSize(gif(16, 9), "gif")).toEqual({ width: 16, height: 9 });
    expect(rasterSize(webp("VP8 ", 300, 200), "webp")).toEqual({ width: 300, height: 200 });
    expect(rasterSize(webp("VP8L", 300, 200), "webp")).toEqual({ width: 300, height: 200 });
    expect(rasterSize(webp("VP8X", 70000, 2), "webp")).toEqual({ width: 70000, height: 2 });
    expect(rasterSize(bmp(10, 20), "bmp")).toEqual({ width: 10, height: 20 });
    expect(rasterSize(svg("<svg/>"), "svg")).toBeNull();
  });

  it("reports damaged headers", () => {
    expect(rasterSize(png(1, 1).slice(0, 20), "png")).toBeNull();
    expect(rasterSize(new Uint8Array([0xff, 0xd8, 0xff, 0xd9, 0, 0, 0, 0, 0, 0, 0, 0]), "jpeg")).toBeNull();
  });

  it("limits decoded pixels", () => {
    expect(sizeProblem(8000, 8000)).toBeNull();
    expect(sizeProblem(8001, 8000)).toMatch(/megapixels/);
    expect(sizeProblem(40000, 10)).toMatch(/40000 × 10 pixels/);
    expect(MAX_IMAGE_PIXELS).toBe(64_000_000);
  });
});

// ----- loading ------------------------------------------------------------------------------------

describe("readProjectImage", () => {
  const created: string[] = [];
  const revoked: string[] = [];
  let decode: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    resetImageCacheForTests();
    let n = 0;
    created.length = 0;
    revoked.length = 0;
    URL.createObjectURL = vi.fn((blob: Blob) => {
      const url = `blob:test-${++n}-${blob.type}`;
      created.push(url);
      return url;
    });
    URL.revokeObjectURL = vi.fn((url: string) => void revoked.push(url));
    decode = vi.fn(async () => ({ width: 4, height: 3, preview: null }));
    setImageDecoderForTests(decode as never);
    ipc.mediaReadImage.mockReset();
    resetImageCacheForTests();
  });
  afterEach(() => setImageDecoderForTests(null));

  it("makes a typed blob URL from the bytes (SVG as image/svg+xml, never markup)", async () => {
    ipc.mediaReadImage.mockResolvedValue(buffer(svg('<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>')));
    const image = await readProjectImage("figs/a.svg", "full");
    expect(image.format).toBe("svg");
    expect(image.url).toMatch(/^blob:test-\d+-image\/svg\+xml$/);
    expect(ipc.mediaReadImage).toHaveBeenCalledWith("figs/a.svg");
    image.release();
    expect(revoked).toContain(image.url);
  });

  it("refuses oversized rasters before decoding them", async () => {
    ipc.mediaReadImage.mockResolvedValue(buffer(png(20000, 20000)));
    await expect(readProjectImage("big.png", "full")).rejects.toMatchObject({ problem: "tooLarge" });
    expect(decode).not.toHaveBeenCalled();
    expect(created).toEqual([]);
  });

  it("names unsupported and damaged files", async () => {
    ipc.mediaReadImage.mockResolvedValue(buffer(svg("just text")));
    await expect(readProjectImage("fake.png", "full")).rejects.toMatchObject({ problem: "unsupported" });
    ipc.mediaReadImage.mockResolvedValue(buffer(png(1, 1).slice(0, 12)));
    await expect(readProjectImage("cut.png", "full")).rejects.toMatchObject({ problem: "damaged" });
    ipc.mediaReadImage.mockResolvedValue(buffer(png(2, 2)));
    decode.mockRejectedValueOnce(new Error("EncodingError"));
    await expect(readProjectImage("bad.png", "full")).rejects.toMatchObject({ problem: "damaged" });
    expect(revoked).toEqual(created); // the failed decode's URL is released
  });

  it("maps backend refusals to concise problems", async () => {
    const refuse = (code: string) => ipc.mediaReadImage.mockRejectedValueOnce({ code, message: code });
    refuse("path.notFound");
    await expect(readProjectImage("a.png", "full")).rejects.toMatchObject({ problem: "missing" });
    refuse("path.outsideWorkspace");
    await expect(readProjectImage("a.png", "full")).rejects.toMatchObject({ problem: "blocked" });
    refuse("image.tooLarge");
    await expect(readProjectImage("a.png", "full")).rejects.toMatchObject({ problem: "tooLarge" });
    refuse("image.unsupported");
    await expect(readProjectImage("a.tiff", "full")).rejects.toMatchObject({ problem: "unsupported" });
    refuse("workspace.changed");
    await expect(readProjectImage("a.png", "full")).rejects.toMatchObject({ problem: "unavailable" });
  });

  it("stops and releases a load that is no longer current", async () => {
    ipc.mediaReadImage.mockResolvedValue(buffer(png(2, 2)));
    let current = true;
    const pending = readProjectImage("a.png", "full", () => current);
    decode.mockImplementationOnce(async () => {
      current = false; // e.g. the project switched while decoding
      return { width: 2, height: 2, preview: null };
    });
    await expect(pending).rejects.toMatchObject({ superseded: true });
    expect(revoked).toEqual(created);
  });

  it("uses the scaled preview for previews and frees the original", async () => {
    ipc.mediaReadImage.mockResolvedValue(buffer(png(2000, 1000)));
    decode.mockResolvedValueOnce({ width: 2000, height: 1000, preview: "blob:preview" });
    const image = await readProjectImage("a.png", "preview");
    expect(decode).toHaveBeenCalledWith(expect.any(String), "png", true);
    expect(image.url).toBe("blob:preview");
    expect(revoked).toEqual(created); // the full-size URL
    image.release();
    expect(revoked).toContain("blob:preview");
  });
});

describe("shared image cache", () => {
  beforeEach(() => {
    resetImageCacheForTests();
    let n = 0;
    URL.createObjectURL = vi.fn(() => `blob:c-${++n}`);
    URL.revokeObjectURL = vi.fn();
    setImageDecoderForTests(async () => ({ width: 1, height: 1, preview: null }));
    ipc.mediaReadImage.mockReset();
    ipc.mediaReadImage.mockImplementation(async () => buffer(png(1, 1)));
    resetImageCacheForTests();
  });
  afterEach(() => setImageDecoderForTests(null));

  it("shares one read between viewers of the same image", async () => {
    const a = acquireImage("x.png");
    const b = acquireImage("x.png");
    expect((await a.promise).url).toBe((await b.promise).url);
    expect(ipc.mediaReadImage).toHaveBeenCalledTimes(1);
    a.release();
    b.release();
  });

  it("reloads after the file changes and tells its viewers", async () => {
    const changed = vi.fn();
    const off = onImageChange("x.png", changed);
    const first = acquireImage("x.png");
    const url1 = (await first.promise).url;
    handleWorkspaceEvent({ kind: "filesChanged", changes: [{ path: "x.png", kind: "modified" }], rescan: false });
    expect(changed).toHaveBeenCalledTimes(1);
    // The shown image keeps its URL until its viewer lets go.
    expect(URL.revokeObjectURL).not.toHaveBeenCalledWith(url1);
    const second = acquireImage("x.png");
    expect((await second.promise).url).not.toBe(url1);
    expect(ipc.mediaReadImage).toHaveBeenCalledTimes(2);
    first.release();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith(url1);
    second.release();
    off();
  });

  it("drops loads of the previous project when another one opens", async () => {
    let resolve!: (b: ArrayBuffer) => void;
    ipc.mediaReadImage.mockImplementationOnce(() => new Promise((r) => (resolve = r)));
    const changed = vi.fn();
    onImageChange("x.png", changed);
    const handle = acquireImage("x.png");
    handleWorkspaceEvent({ kind: "closed" });
    resolve(buffer(png(1, 1)));
    await expect(handle.promise).rejects.toBeInstanceOf(ImageLoadError);
    await expect(handle.promise).rejects.toMatchObject({ superseded: true });
    expect(changed).toHaveBeenCalled();
    handle.release();
  });

  it("discards a load whose last viewer left before it finished", async () => {
    let resolve!: (b: ArrayBuffer) => void;
    ipc.mediaReadImage.mockImplementationOnce(() => new Promise((r) => (resolve = r)));
    const handle = acquireImage("slow.png");
    handle.release();
    resolve(buffer(png(1, 1)));
    await expect(handle.promise).rejects.toMatchObject({ superseded: true });
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });

  it("reads at most three images at once", async () => {
    const resolvers: (() => void)[] = [];
    ipc.mediaReadImage.mockImplementation(
      () => new Promise((r) => resolvers.push(() => r(buffer(png(1, 1))))),
    );
    const handles = ["1", "2", "3", "4", "5"].map((n) => acquireImage(`${n}.png`));
    await Promise.resolve();
    await Promise.resolve();
    expect(ipc.mediaReadImage).toHaveBeenCalledTimes(3);
    resolvers.shift()!();
    await handles[0]!.promise;
    await vi.waitFor(() => expect(ipc.mediaReadImage).toHaveBeenCalledTimes(4));
    resolvers.forEach((r) => r());
    handles.forEach((h) => h.release());
  });
});
