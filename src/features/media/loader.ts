// The one safe loader for project images, shared by the image editor tab and
// inline image previews in source.
//
// Security and resource rules (see also src-tauri/src/media.rs):
// - Bytes come only from `media_read_image`: image files inside the open
//   project (extension allow-list, links/junctions out of the folder refused,
//   at most 32 MiB). Nothing is fetched from the network, and no path outside
//   the project is ever read.
// - Every image, SVG included, is displayed ONLY through an <img> element whose
//   src is a blob: URL created here. An SVG in an <img> is inert: its scripts
//   never run, it cannot load external resources (also blocked by the CSP
//   `img-src 'self' data: blob:`), and it has no access to the application.
//   SVG markup is never inserted into the DOM (no innerHTML, <object>, <iframe>).
// - The format is decided from the bytes (not the extension). Raster
//   dimensions are read from the file header and checked against the pixel
//   limits BEFORE anything is decoded; decoding is asynchronous (img.decode).
// - Loads are cached per project path, shared by every viewer, discarded when
//   nobody needs them anymore (late results are dropped and their URLs
//   revoked), refreshed when the file changes, and cleared when the project
//   opens/closes so no image of another project is ever shown.
import type { WorkspaceEvent } from "../../bindings/WorkspaceEvent";
import { commands, events, isCommandError } from "../../lib/ipc";

export const MAX_IMAGE_BYTES = 32 * 1024 * 1024;
/** Largest raster image decoded (pixels). */
export const MAX_IMAGE_PIXELS = 64_000_000;
/** Largest raster side decoded (pixels). */
export const MAX_IMAGE_SIDE = 32_767;
/** Inline previews are scaled to fit this many pixels on their longer side. */
export const PREVIEW_MAX_SIDE = 512;
/** Image reads running at once (more wait in a queue). */
const MAX_CONCURRENT_READS = 3;
/** Loaded images kept after their last viewer is gone. */
const IDLE_CACHE = 16;

export type ImageFormat = "png" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "avif" | "ico";

export const FORMAT_LABELS: Record<ImageFormat, string> = {
  png: "PNG",
  jpeg: "JPEG",
  gif: "GIF",
  webp: "WebP",
  bmp: "BMP",
  svg: "SVG",
  avif: "AVIF",
  ico: "ICO",
};

const MEDIA_TYPES: Record<ImageFormat, string> = {
  png: "image/png",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  svg: "image/svg+xml",
  avif: "image/avif",
  ico: "image/x-icon",
};

/** Extensions the image tab opens (displayable ones and known undisplayable image formats). */
export const IMAGE_TAB_EXTENSIONS = new Set([
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "bmp",
  "svg",
  "avif",
  "ico",
  "tif",
  "tiff",
  "heic",
  "heif",
]);

export type ImageProblem = "missing" | "blocked" | "tooLarge" | "unsupported" | "damaged" | "unavailable";

export class ImageLoadError extends Error {
  constructor(
    readonly problem: ImageProblem,
    message: string,
    /** The load was dropped (file changed, project switched, nobody needs it); not a result. */
    readonly superseded = false,
  ) {
    super(message);
    this.name = "ImageLoadError";
  }
}

export interface LoadedImage {
  /** blob: URL to use as an <img> src (owned by the loader; never revoke it yourself). */
  url: string;
  format: ImageFormat;
  /** Intrinsic size of the original image (SVG without one: its rendered default). */
  width: number;
  height: number;
  /** File size in bytes. */
  bytes: number;
}

// ----- format detection --------------------------------------------------------------------

const ascii = (b: Uint8Array, at: number, text: string) =>
  text.split("").every((c, i) => b[at + i] === c.charCodeAt(0));

/** The image format of `bytes`, from their content; null when not a supported image. */
export function sniffImage(b: Uint8Array): ImageFormat | null {
  if (b.length >= 8 && b[0] === 0x89 && ascii(b, 1, "PNG\r\n\x1a\n")) return "png";
  if (b.length >= 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return "jpeg";
  if (b.length >= 6 && (ascii(b, 0, "GIF87a") || ascii(b, 0, "GIF89a"))) return "gif";
  if (b.length >= 12 && ascii(b, 0, "RIFF") && ascii(b, 8, "WEBP")) return "webp";
  if (b.length >= 26 && ascii(b, 0, "BM")) return "bmp";
  if (b.length >= 12 && ascii(b, 4, "ftyp") && (ascii(b, 8, "avif") || ascii(b, 8, "avis"))) return "avif";
  if (b.length >= 22 && b[0] === 0 && b[1] === 0 && b[2] === 1 && b[3] === 0 && b[4]! + b[5]! > 0) return "ico";
  if (looksLikeSvg(b)) return "svg";
  return null;
}

function looksLikeSvg(b: Uint8Array): boolean {
  const head = b.subarray(0, 64 * 1024);
  if (head.includes(0)) return false;
  const text = new TextDecoder("utf-8", { fatal: false }).decode(head).replace(/^﻿/, "").trimStart();
  return text.startsWith("<") && /<svg[\s>/]/i.test(text);
}

const u16be = (b: Uint8Array, i: number) => (b[i]! << 8) | b[i + 1]!;
const u16le = (b: Uint8Array, i: number) => b[i]! | (b[i + 1]! << 8);
const u24le = (b: Uint8Array, i: number) => b[i]! | (b[i + 1]! << 8) | (b[i + 2]! << 16);
const u32be = (b: Uint8Array, i: number) => ((b[i]! << 24) >>> 0) + ((b[i + 1]! << 16) | (b[i + 2]! << 8) | b[i + 3]!);
const i32le = (b: Uint8Array, i: number) => b[i]! | (b[i + 1]! << 8) | (b[i + 2]! << 16) | (b[i + 3]! << 24);

/**
 * Pixel size of a raster image from its header, without decoding it. Null when
 * the header is damaged or (SVG) there is no raster size; undefined when the
 * format stores it somewhere this reader does not look (checked after decoding).
 */
export function rasterSize(b: Uint8Array, format: ImageFormat): { width: number; height: number } | null | undefined {
  switch (format) {
    case "png":
      return b.length >= 24 && ascii(b, 12, "IHDR") ? { width: u32be(b, 16), height: u32be(b, 20) } : null;
    case "gif":
      return b.length >= 10 ? { width: u16le(b, 6), height: u16le(b, 8) } : null;
    case "jpeg": {
      let i = 2;
      while (i + 9 < b.length) {
        if (b[i] !== 0xff) return null;
        const marker = b[i + 1]!;
        if (marker === 0xff) {
          i += 1;
          continue;
        }
        if (marker === 0xd8 || marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) {
          i += 2;
          continue;
        }
        const isFrame = marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc;
        if (isFrame) return { height: u16be(b, i + 5), width: u16be(b, i + 7) };
        if (marker === 0xd9 || marker === 0xda) return null;
        i += 2 + u16be(b, i + 2);
      }
      return null;
    }
    case "webp": {
      if (b.length < 30) return null;
      if (ascii(b, 12, "VP8 ")) return { width: u16le(b, 26) & 0x3fff, height: u16le(b, 28) & 0x3fff };
      if (ascii(b, 12, "VP8L")) {
        const [b0, b1, b2, b3] = [b[21]!, b[22]!, b[23]!, b[24]!];
        return { width: 1 + (((b1 & 0x3f) << 8) | b0), height: 1 + (((b3 & 0x0f) << 10) | (b2 << 2) | ((b1 & 0xc0) >> 6)) };
      }
      if (ascii(b, 12, "VP8X")) return { width: 1 + u24le(b, 24), height: 1 + u24le(b, 27) };
      return null;
    }
    case "bmp": {
      const header = b[14]! | (b[15]! << 8);
      if (header === 12) return { width: u16le(b, 18), height: u16le(b, 20) };
      return { width: Math.abs(i32le(b, 18)), height: Math.abs(i32le(b, 22)) };
    }
    case "ico": {
      const count = u16le(b, 4);
      let width = 0;
      let height = 0;
      for (let k = 0; k < count && 6 + k * 16 + 1 < b.length; k++) {
        width = Math.max(width, b[6 + k * 16] || 256);
        height = Math.max(height, b[7 + k * 16] || 256);
      }
      return count > 0 ? { width, height } : null;
    }
    case "avif": {
      const limit = Math.min(b.length - 16, 64 * 1024);
      for (let i = 4; i < limit; i++) {
        if (ascii(b, i, "ispe")) return { width: u32be(b, i + 8), height: u32be(b, i + 12) };
      }
      return undefined;
    }
    case "svg":
      return null;
  }
}

/** Why a raster of this size is not decoded, or null when it may be. */
export function sizeProblem(width: number, height: number): string | null {
  if (width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE || width * height > MAX_IMAGE_PIXELS) {
    return `${width} × ${height} pixels; images up to ${MAX_IMAGE_PIXELS / 1_000_000} megapixels are shown.`;
  }
  return null;
}

// ----- decoding (replaceable in unit tests) -------------------------------------------------

export interface Decoded {
  width: number;
  height: number;
  /** A smaller blob: URL for previews, or null to use the original. */
  preview: string | null;
}

export type Decoder = (url: string, format: ImageFormat, wantPreview: boolean) => Promise<Decoded>;

const browserDecoder: Decoder = async (url, format, wantPreview) => {
  const img = new Image();
  img.decoding = "async";
  img.src = url;
  await img.decode();
  const width = img.naturalWidth || 300;
  const height = img.naturalHeight || 150;
  if (!wantPreview || format === "svg" || Math.max(width, height) <= PREVIEW_MAX_SIDE) {
    return { width, height, preview: null };
  }
  const scale = PREVIEW_MAX_SIDE / Math.max(width, height);
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(width * scale));
  canvas.height = Math.max(1, Math.round(height * scale));
  canvas.getContext("2d")?.drawImage(img, 0, 0, canvas.width, canvas.height);
  const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
  return { width, height, preview: blob ? URL.createObjectURL(blob) : null };
};

let decoder: Decoder = browserDecoder;

/** Test hook: replace image decoding (jsdom cannot decode). */
export function setImageDecoderForTests(next: Decoder | null): void {
  decoder = next ?? browserDecoder;
}

// ----- loading -------------------------------------------------------------------------------

function problemOf(error: unknown): ImageLoadError {
  if (error instanceof ImageLoadError) return error;
  if (isCommandError(error)) {
    const map: Record<string, ImageProblem> = {
      "path.notFound": "missing",
      "path.brokenLink": "missing",
      "file.isDirectory": "missing",
      "path.outsideWorkspace": "blocked",
      "image.tooLarge": "tooLarge",
      "image.unsupported": "unsupported",
      "image.notImage": "unsupported",
    };
    return new ImageLoadError(map[error.code] ?? "unavailable", error.message);
  }
  return new ImageLoadError("unavailable", error instanceof Error ? error.message : String(error));
}

let active = 0;
const waiting: (() => void)[] = [];

async function withReadSlot<T>(run: () => Promise<T>): Promise<T> {
  if (active >= MAX_CONCURRENT_READS) await new Promise<void>((resolve) => waiting.push(resolve));
  active += 1;
  try {
    return await run();
  } finally {
    active -= 1;
    waiting.shift()?.();
  }
}

/**
 * Read and decode one project image. `isCurrent` is consulted after every
 * asynchronous step; once it returns false the load stops and its resources
 * are released (the caller is gone, the file changed, or the project switched).
 */
export async function readProjectImage(
  path: string,
  variant: "full" | "preview",
  isCurrent: () => boolean = () => true,
): Promise<LoadedImage & { release: () => void }> {
  const stale = () => new ImageLoadError("unavailable", "The image load was superseded.", true);
  let buffer: ArrayBuffer;
  try {
    buffer = await withReadSlot(() => (isCurrent() ? commands.mediaReadImage(path) : Promise.reject(stale())));
  } catch (error) {
    throw problemOf(error);
  }
  if (!isCurrent()) throw stale();
  const bytes = new Uint8Array(buffer);
  if (bytes.byteLength > MAX_IMAGE_BYTES) throw new ImageLoadError("tooLarge", `${path} is larger than 32 MB.`);
  const format = sniffImage(bytes);
  if (!format) {
    throw new ImageLoadError(
      bytes.byteLength === 0 ? "damaged" : "unsupported",
      bytes.byteLength === 0 ? "The file is empty." : "The file is not a PNG, JPEG, GIF, WebP, BMP, SVG, AVIF, or ICO image.",
    );
  }
  const header = rasterSize(bytes, format);
  if (header === null && format !== "svg") throw new ImageLoadError("damaged", `The ${FORMAT_LABELS[format]} file is damaged.`);
  if (header) {
    const problem = sizeProblem(header.width, header.height);
    if (problem) throw new ImageLoadError("tooLarge", problem);
  }
  const url = URL.createObjectURL(new Blob([bytes], { type: MEDIA_TYPES[format] }));
  let decoded: Decoded;
  try {
    decoded = await decoder(url, format, variant === "preview");
  } catch {
    URL.revokeObjectURL(url);
    throw new ImageLoadError("damaged", `The ${FORMAT_LABELS[format]} file is damaged or cannot be decoded.`);
  }
  const release = () => {
    URL.revokeObjectURL(url);
    if (decoded.preview) URL.revokeObjectURL(decoded.preview);
  };
  if (!isCurrent()) {
    release();
    throw stale();
  }
  const problem = format === "svg" ? null : sizeProblem(decoded.width, decoded.height);
  if (problem) {
    release();
    throw new ImageLoadError("tooLarge", problem);
  }
  if (decoded.preview) {
    // The preview replaces the original: free the full-size bytes at once.
    URL.revokeObjectURL(url);
    const preview = decoded.preview;
    return {
      url: preview,
      format,
      width: decoded.width,
      height: decoded.height,
      bytes: bytes.byteLength,
      release: () => URL.revokeObjectURL(preview),
    };
  }
  return { url, format, width: decoded.width, height: decoded.height, bytes: bytes.byteLength, release };
}

// ----- shared cache with change tracking ----------------------------------------------------

type Settled = { ok: true; image: LoadedImage } | { ok: false; error: ImageLoadError };

interface Entry {
  key: string;
  path: string;
  promise: Promise<Settled>;
  settled: Settled | null;
  release: (() => void) | null;
  refs: number;
  dead: boolean;
}

const entries = new Map<string, Entry>();
/** Unreferenced, settled entries in least-recently-used order. */
const idle: string[] = [];
const listeners = new Map<string, Set<() => void>>();
let projectGeneration = 0;

function drop(entry: Entry) {
  entry.dead = true;
  if (entries.get(entry.key) === entry) entries.delete(entry.key);
  const i = idle.indexOf(entry.key);
  if (i >= 0) idle.splice(i, 1);
  if (entry.refs === 0) entry.release?.();
}

function load(path: string, variant: "full" | "preview"): Entry {
  const key = `${variant}:${path}`;
  const existing = entries.get(key);
  if (existing) return existing;
  const generation = projectGeneration;
  const entry: Entry = { key, path, promise: Promise.resolve(null as never), settled: null, release: null, refs: 0, dead: false };
  // A load stops once it is dropped: its last viewer released it before it
  // settled, the file changed, or the project switched.
  const isCurrent = () => !entry.dead && generation === projectGeneration;
  entry.promise = readProjectImage(path, variant, isCurrent).then(
    ({ release, ...image }) => {
      entry.release = release;
      if (entry.dead && entry.refs === 0) release();
      return (entry.settled = { ok: true, image });
    },
    (error: unknown) => (entry.settled = { ok: false, error: problemOf(error) }),
  );
  entries.set(key, entry);
  return entry;
}

export interface ImageHandle {
  /** Resolves with the image or rejects with an `ImageLoadError`. */
  promise: Promise<LoadedImage>;
  /** The caller no longer shows the image. */
  release: () => void;
}

/** Load (or share) a project image. Call `release` when it is no longer shown. */
export function acquireImage(path: string, variant: "full" | "preview" = "full"): ImageHandle {
  ensureWatching();
  const entry = load(path, variant);
  entry.refs += 1;
  const i = idle.indexOf(entry.key);
  if (i >= 0) idle.splice(i, 1);
  let released = false;
  const promise = entry.promise.then((s) => {
    if (s.ok) return s.image;
    // A superseded load is retried by whoever still needs it (see onImageChange).
    throw s.error;
  });
  promise.catch(() => {});
  return {
    promise,
    release: () => {
      if (released) return;
      released = true;
      entry.refs -= 1;
      if (entry.refs > 0) return;
      if (entry.dead || !entry.settled?.ok) {
        drop(entry);
        return;
      }
      idle.push(entry.key);
      while (idle.length > IDLE_CACHE) {
        const old = entries.get(idle.shift()!);
        if (old) drop(old);
      }
    },
  };
}

/** Be told when the image at `path` changed on disk (or the project changed): load it again. */
export function onImageChange(path: string, listener: () => void): () => void {
  ensureWatching();
  let set = listeners.get(path);
  if (!set) listeners.set(path, (set = new Set()));
  set.add(listener);
  return () => {
    set.delete(listener);
    if (set.size === 0) listeners.delete(path);
  };
}

function notify(path: string) {
  for (const listener of [...(listeners.get(path) ?? [])]) listener();
}

/** Forget cached loads of `path` and tell its viewers to reload. */
export function invalidateImage(path: string): void {
  for (const entry of [...entries.values()]) if (entry.path === path) drop(entry);
  notify(path);
}

/** Forget every cached load (project opened/closed, missed watcher events). */
export function invalidateAllImages(projectChanged: boolean): void {
  if (projectChanged) projectGeneration += 1;
  for (const entry of [...entries.values()]) drop(entry);
  for (const path of [...listeners.keys()]) notify(path);
}

/** Workspace events that make cached images stale. Exported for tests. */
export function handleWorkspaceEvent(event: WorkspaceEvent): void {
  if (event.kind === "opened" || event.kind === "closed") invalidateAllImages(true);
  else if (event.kind === "filesChanged") {
    if (event.rescan) invalidateAllImages(false);
    for (const change of event.changes) invalidateImage(change.path);
  }
}

let watching = false;

function ensureWatching() {
  if (watching) return;
  watching = true;
  try {
    void events.workspace(handleWorkspaceEvent).catch(() => {});
  } catch {
    // Not running inside the desktop shell (unit tests).
  }
}

/** Test hook: forget all state. */
export function resetImageCacheForTests(): void {
  for (const entry of [...entries.values()]) drop(entry);
  idle.length = 0;
  listeners.clear();
  projectGeneration += 1;
}

/** "245 KB", "3.4 MB". */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
