// Inline image previews in Markdown, LaTeX, and Typst source (Monaco view
// zones). View-only: previews never edit the text, never mark the document
// dirty, never add undo entries, and never trigger a build. They work without
// any typesetting toolchain.
//
// - Recognition: the backend runs the file's own graph extractor over the
//   CURRENT buffer text (unsaved edits included), so comments, verbatim/code,
//   and non-literal (macro or variable) paths are treated exactly as project
//   indexing treats them, and paths resolve with the same rules, inside the
//   open project only. Nothing is stored in the graph by previewing.
// - Loading: the shared safe loader (loader.ts), lazily when a preview scrolls
//   into view, scaled down for display, released when it leaves the editor.
// - Visibility: per image (Show/Hide) and per document (palette and editor
//   context menu), remembered locally per project file.
import { EyeOff, Image as ImageIcon, ImageOff, ShieldAlert } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";

import type { ImageReference } from "../../bindings/ImageReference";
import type { WorkspaceEvent } from "../../bindings/WorkspaceEvent";
import { commands, events } from "../../lib/ipc";
import type { monaco } from "../../lib/monaco";
import { parseJson, readStored, writeStored } from "../../ui/storage";
import { Button, Spinner } from "../../ui/components";
import type { DocumentHandle } from "../editor/documentClient";
import { useWorkspace } from "../workspace/store";
import { FORMAT_LABELS } from "./loader";
import { useProjectImage } from "./useProjectImage";
import "./media.css";

/** Source files whose image references are previewed. */
const PREVIEW_EXTENSIONS = new Set(["md", "markdown", "tex", "ltx", "typ"]);

export function supportsSourcePreviews(path: string | null | undefined): boolean {
  const name = path?.split("/").pop() ?? "";
  const dot = name.lastIndexOf(".");
  return dot > 0 && PREVIEW_EXTENSIONS.has(name.slice(dot + 1).toLowerCase());
}

/** Zone heights (px): a preview with its picture, and a one-line state. */
export const PREVIEW_HEIGHT = 156;
export const LINE_HEIGHT = 30;
/** References previewed per document. */
const MAX_PREVIEWS = 200;
const REFRESH_DELAY_MS = 300;

// ----- preferences (local view state) -------------------------------------------------------

const PREFS_KEY = "exposition.sourceImagePreviews";
const MAX_DOCS = 200;

interface DocPrefs {
  /** All previews of this document hidden. */
  off?: boolean;
  /** Written paths whose preview is hidden. */
  hidden?: string[];
  /** Last change (for pruning). */
  t: number;
}

interface StoredPrefs {
  v: 1;
  docs: Record<string, DocPrefs>;
}

function readPrefs(): StoredPrefs {
  const raw = parseJson(readStored(PREFS_KEY));
  if (raw && typeof raw === "object" && (raw as StoredPrefs).v === 1 && typeof (raw as StoredPrefs).docs === "object") {
    return raw as StoredPrefs;
  }
  return { v: 1, docs: {} };
}

const prefListeners = new Set<() => void>();

/** Per-document preview visibility, stored with the other local view preferences. */
export const previewPrefs = {
  docKey(path: string): string {
    return `${useWorkspace.getState().info?.root ?? ""}|${path}`;
  },
  get(doc: string): DocPrefs {
    return readPrefs().docs[doc] ?? { t: 0 };
  },
  isOff(doc: string): boolean {
    return Boolean(previewPrefs.get(doc).off);
  },
  isHidden(doc: string, written: string): boolean {
    return previewPrefs.get(doc).hidden?.includes(written) ?? false;
  },
  update(doc: string, change: (p: DocPrefs) => DocPrefs): void {
    const prefs = readPrefs();
    const next = change({ ...(prefs.docs[doc] ?? { t: 0 }) });
    next.t = Date.now();
    if (!next.off && !next.hidden?.length) delete prefs.docs[doc];
    else prefs.docs[doc] = next;
    const keys = Object.keys(prefs.docs);
    if (keys.length > MAX_DOCS) {
      keys
        .sort((a, b) => prefs.docs[a]!.t - prefs.docs[b]!.t)
        .slice(0, keys.length - MAX_DOCS)
        .forEach((k) => delete prefs.docs[k]);
    }
    writeStored(PREFS_KEY, JSON.stringify(prefs));
    for (const listener of [...prefListeners]) listener();
  },
  setOff(doc: string, off: boolean): void {
    previewPrefs.update(doc, (p) => ({ ...p, off: off || undefined }));
  },
  setHidden(doc: string, written: string, hidden: boolean): void {
    previewPrefs.update(doc, (p) => {
      const set = new Set(p.hidden ?? []);
      if (hidden) set.add(written);
      else set.delete(written);
      return { ...p, hidden: [...set].slice(-500) };
    });
  },
  subscribe(listener: () => void): () => void {
    prefListeners.add(listener);
    return () => prefListeners.delete(listener);
  },
};

// ----- planning (pure) ----------------------------------------------------------------------

export interface PlannedZone {
  /** Stable while the reference exists: the written path and its occurrence number. */
  key: string;
  afterLineNumber: number;
  reference: ImageReference;
  hidden: boolean;
  /** One line (hidden, unresolved, or not displayable) or a picture. */
  compact: boolean;
}

/**
 * The view zones for `references`: one after the line where each reference
 * ends, in source order. `lineOf` maps a UTF-16 offset to a line number.
 */
export function planZones(
  references: ImageReference[],
  lineOf: (offset: number) => number,
  isHidden: (written: string) => boolean,
  failed: ReadonlySet<string> = new Set(),
): PlannedZone[] {
  const seen = new Map<string, number>();
  return references.slice(0, MAX_PREVIEWS).map((reference) => {
    const n = (seen.get(reference.written) ?? 0) + 1;
    seen.set(reference.written, n);
    const key = `${reference.written}#${n}`;
    const hidden = isHidden(reference.written);
    return {
      key,
      afterLineNumber: lineOf(reference.range.end),
      reference,
      hidden,
      compact: hidden || reference.location.state !== "found" || failed.has(key),
    };
  });
}

// ----- one preview ------------------------------------------------------------------------------

interface PreviewProps {
  zone: PlannedZone;
  onToggle: (written: string, hide: boolean) => void;
  /** The image could not be displayed (shrink to one line). */
  onFailed: (key: string, failed: boolean) => void;
}

function StateLine({
  icon: Icon,
  text,
  written,
  action,
}: {
  icon: typeof ImageIcon;
  text: string;
  written: string;
  action: ReactNode;
}) {
  return (
    <div className="xp-source-image__line">
      <Icon size={14} aria-hidden />
      <span className="xp-source-image__label" title={written}>
        {text}
        <span className="ui-code">{written}</span>
      </span>
      {action}
    </div>
  );
}

export function SourceImagePreview({ zone, onToggle, onFailed }: PreviewProps) {
  const { reference, hidden } = zone;
  const location = reference.location;
  const box = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);
  const path = location.state === "found" ? location.path : null;
  // Load only when shown on screen (and not hidden).
  useEffect(() => {
    const el = box.current;
    if (!el || hidden || !path) return;
    if (typeof IntersectionObserver === "undefined") {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver((entries) => setVisible(entries.some((e) => e.isIntersecting)), {
      rootMargin: "200px 0px",
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [hidden, path]);
  const image = useProjectImage(path, "preview", visible && !hidden);
  const failed = image.status === "error";
  useEffect(() => onFailed(zone.key, failed), [failed, zone.key]);

  const hide = (
    <Button size="sm" variant="ghost" onPress={() => onToggle(reference.written, true)} aria-label={`Hide preview of ${reference.written}`}>
      Hide
    </Button>
  );
  let body: ReactNode;
  if (hidden) {
    body = (
      <StateLine
        icon={EyeOff}
        text="Preview hidden: "
        written={reference.written}
        action={
          <Button size="sm" variant="ghost" onPress={() => onToggle(reference.written, false)} aria-label={`Show preview of ${reference.written}`}>
            Show
          </Button>
        }
      />
    );
  } else if (location.state === "missing") {
    body = <StateLine icon={ImageOff} text="Image not found: " written={reference.written} action={hide} />;
  } else if (location.state === "blocked") {
    body = <StateLine icon={ShieldAlert} text="Outside the project, not shown: " written={reference.written} action={hide} />;
  } else if (location.state === "unsupported") {
    const ext = location.path.split(".").pop()?.toUpperCase() ?? "";
    body = <StateLine icon={ImageOff} text={`No preview for ${ext} files: `} written={reference.written} action={hide} />;
  } else if (image.status === "error") {
    const text =
      image.error.problem === "tooLarge"
        ? "Image too large to preview: "
        : image.error.problem === "missing"
          ? "Image not found: "
          : image.error.problem === "unsupported"
            ? "Unsupported image format: "
            : image.error.problem === "damaged"
              ? "Damaged image: "
              : "Could not load: ";
    body = <StateLine icon={ImageOff} text={text} written={reference.written} action={hide} />;
  } else {
    const ready = image.status === "ready" ? image.image : null;
    body = (
      <>
        <div className="xp-source-image__line">
          <ImageIcon size={14} aria-hidden />
          <span className="xp-source-image__label ui-code" title={location.path}>
            {location.path}
          </span>
          {ready && (
            <span className="xp-source-image__label">
              {`${ready.width} × ${ready.height} · ${FORMAT_LABELS[ready.format]}`}
            </span>
          )}
          {hide}
        </div>
        <div className="xp-source-image__frame">
          {ready ? (
            <img
              className="xp-source-image__image"
              src={ready.url}
              alt={`Preview of ${location.path}`}
              draggable={false}
              data-testid="source-image-preview-image"
            />
          ) : (
            <Spinner size="sm" aria-label={`Loading a preview of ${location.path}`} />
          )}
        </div>
      </>
    );
  }
  return (
    <div
      ref={box}
      className="xp-source-image"
      data-testid="source-image-preview"
      data-written={reference.written}
      data-state={hidden ? "hidden" : location.state === "found" ? (image.status === "error" ? image.error.problem : image.status) : location.state}
      data-path={path ?? undefined}
    >
      {body}
    </div>
  );
}

// ----- the editor contribution ---------------------------------------------------------------

interface LiveZone {
  id: string;
  zone: monaco.editor.IViewZone;
  root: Root;
  plan: PlannedZone;
}

/** A source editor showing previews; the commands act on the focused/last active one. */
const controllers = new Set<PreviewController>();
let lastActive: PreviewController | null = null;

export class PreviewController {
  private zones = new Map<string, LiveZone>();
  private references: ImageReference[] = [];
  private failed = new Set<string>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private request = 0;
  private disposed = false;
  private cleanups: (() => void)[] = [];
  /** Told when document-wide previews turn on or off (editor context keys). */
  onEnabled: ((enabled: boolean) => void) | null = null;

  constructor(
    readonly editor: monaco.editor.IStandaloneCodeEditor,
    private readonly handle: Pick<DocumentHandle, "path" | "model">,
    private readonly query: (path: string, text: string) => Promise<ImageReference[]> = commands.mediaImageReferences,
  ) {
    controllers.add(this);
    const model = handle.model;
    const subs = [
      model.onDidChangeContent(() => this.schedule(REFRESH_DELAY_MS)),
      editor.onDidFocusEditorWidget(() => (lastActive = this)),
    ];
    this.cleanups.push(() => subs.forEach((s) => s.dispose()));
    this.cleanups.push(previewPrefs.subscribe(() => this.onPrefs()));
    watchWorkspace();
    this.schedule(0);
  }

  get docKey(): string | null {
    return this.handle.path ? previewPrefs.docKey(this.handle.path) : null;
  }

  get enabled(): boolean {
    const doc = this.docKey;
    return Boolean(doc && supportsSourcePreviews(this.handle.path) && !previewPrefs.isOff(doc));
  }

  /** Show or hide every preview of this document. */
  setDocumentPreviews(show: boolean): void {
    const doc = this.docKey;
    if (doc) previewPrefs.setOff(doc, !show);
  }

  /** Show or hide the preview of the reference at the cursor (true when there is one). */
  toggleAtCursor(): boolean {
    const position = this.editor.getPosition();
    const doc = this.docKey;
    if (!position || !doc) return false;
    const offset = this.handle.model.getOffsetAt(position);
    const line = position.lineNumber;
    const hit =
      this.references.find((r) => r.range.start <= offset && offset <= r.range.end) ??
      this.references.find((r) => this.lineOf(r.range.end) === line);
    if (!hit) return false;
    if (previewPrefs.isOff(doc)) {
      previewPrefs.setOff(doc, false);
      previewPrefs.setHidden(doc, hit.written, false);
    } else {
      previewPrefs.setHidden(doc, hit.written, !previewPrefs.isHidden(doc, hit.written));
    }
    return true;
  }

  private lineOf(offset: number): number {
    const model = this.handle.model;
    return model.getPositionAt(Math.min(offset, model.getValueLength())).lineNumber;
  }

  private schedule(delay: number) {
    if (this.disposed) return;
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => void this.refresh(), delay);
  }

  private onPrefs() {
    this.onEnabled?.(this.enabled);
    if (!this.enabled) this.apply([]);
    else this.schedule(0);
  }

  /** Another project was opened, or this one closed: never keep its previews. */
  projectChanged(): void {
    this.references = [];
    this.apply([]);
    this.schedule(0);
  }

  /** Image files (or folders) of the project changed: locate the references again. */
  imagesChanged(): void {
    this.schedule(150);
  }

  /** Ask the backend for the references of the current text; drop obsolete answers. */
  async refresh(): Promise<void> {
    this.timer = null;
    if (this.disposed) return;
    const path = this.handle.path;
    if (!this.enabled || !path) {
      this.apply([]);
      return;
    }
    const model = this.handle.model;
    const version = model.getVersionId();
    const request = ++this.request;
    let references: ImageReference[];
    try {
      references = await this.query(path, model.getValue());
    } catch {
      references = [];
    }
    // A newer edit or request superseded this answer (a refresh is scheduled).
    if (this.disposed || request !== this.request || model.getVersionId() !== version || path !== this.handle.path) return;
    this.references = references;
    this.render();
  }

  private render() {
    const doc = this.docKey;
    if (!doc || !this.enabled) {
      this.apply([]);
      return;
    }
    const hidden = new Set(previewPrefs.get(doc).hidden ?? []);
    this.apply(planZones(this.references, (o) => this.lineOf(o), (w) => hidden.has(w), this.failed));
  }

  private onToggle = (written: string, hide: boolean) => {
    const doc = this.docKey;
    if (doc) previewPrefs.setHidden(doc, written, hide);
  };

  private onFailed = (key: string, failed: boolean) => {
    if (failed === this.failed.has(key)) return;
    if (failed) this.failed.add(key);
    else this.failed.delete(key);
    // Defer: this runs during a preview's render cycle.
    queueMicrotask(() => !this.disposed && this.render());
  };

  /** Make the editor's zones match `plan` (view-only: the model is never touched). */
  private apply(plan: PlannedZone[]) {
    const wanted = new Map(plan.map((p) => [p.key, p]));
    const unmount: Root[] = [];
    this.editor.changeViewZones((accessor) => {
      for (const [key, live] of this.zones) {
        if (!wanted.has(key)) {
          accessor.removeZone(live.id);
          unmount.push(live.root);
          this.zones.delete(key);
        }
      }
      for (const p of plan) {
        const height = p.compact ? LINE_HEIGHT : PREVIEW_HEIGHT;
        const live = this.zones.get(p.key);
        if (live) {
          const moved = live.zone.afterLineNumber !== p.afterLineNumber || live.zone.heightInPx !== height;
          live.zone.afterLineNumber = p.afterLineNumber;
          live.zone.heightInPx = height;
          live.plan = p;
          if (moved) accessor.layoutZone(live.id);
          this.renderZone(live);
          continue;
        }
        const domNode = document.createElement("div");
        domNode.className = "xp-source-image-zone";
        const zone: monaco.editor.IViewZone = {
          afterLineNumber: p.afterLineNumber,
          heightInPx: height,
          domNode,
          // Clicks in a preview never move the caret or start a selection.
          suppressMouseDown: false,
        };
        const id = accessor.addZone(zone);
        const created: LiveZone = { id, zone, root: createRoot(domNode), plan: p };
        this.zones.set(p.key, created);
        this.renderZone(created);
      }
    });
    if (unmount.length) queueMicrotask(() => unmount.forEach((root) => root.unmount()));
  }

  private renderZone(live: LiveZone) {
    live.root.render(<SourceImagePreview zone={live.plan} onToggle={this.onToggle} onFailed={this.onFailed} />);
  }

  dispose(): void {
    if (this.disposed) return;
    this.apply([]);
    this.disposed = true;
    if (this.timer) clearTimeout(this.timer);
    this.cleanups.forEach((c) => c());
    controllers.delete(this);
    unwatchWorkspace();
    if (lastActive === this) lastActive = null;
  }
}

/**
 * One workspace-event subscription for every preview controller (there is a
 * controller per open source editor): each event is examined once, then the
 * controllers are told. Held only while a controller exists.
 */
let workspaceWatch: Promise<() => void> | null = null;

/** Tell the preview controllers about a workspace event. */
export function dispatchWorkspaceEvent(event: WorkspaceEvent): void {
  if (event.kind === "opened" || event.kind === "closed") {
    for (const controller of [...controllers]) controller.projectChanged();
    return;
  }
  if (event.kind !== "filesChanged") return;
  const relevant =
    event.rescan ||
    event.changes.some((c) => /\.(png|jpe?g|gif|webp|bmp|svg|avif|ico|pdf|eps|tiff?)$/i.test(c.path) || !c.path.includes("."));
  if (relevant) for (const controller of [...controllers]) controller.imagesChanged();
}

function watchWorkspace() {
  if (workspaceWatch) return;
  try {
    const pending = events.workspace(dispatchWorkspaceEvent);
    workspaceWatch = pending;
    // A subscription that failed is forgotten, so the next controller tries again.
    pending.catch(() => {
      if (workspaceWatch === pending) workspaceWatch = null;
    });
  } catch {
    // Not running inside the desktop shell (unit tests).
  }
}

function unwatchWorkspace() {
  if (controllers.size > 0 || !workspaceWatch) return;
  const pending = workspaceWatch;
  workspaceWatch = null;
  void pending.then((off) => off()).catch(() => {});
}

/** The preview controller of the focused (or most recently focused) source editor. */
export function activePreviewController(): PreviewController | null {
  for (const c of controllers) if (c.editor.hasTextFocus()) return c;
  return lastActive && controllers.has(lastActive) ? lastActive : null;
}

/** Source editor contribution: previews for Markdown, LaTeX, and Typst files. */
export function attachImagePreviews(
  editor: monaco.editor.IStandaloneCodeEditor,
  handle: DocumentHandle,
): monaco.IDisposable {
  if (!supportsSourcePreviews(handle.path)) return { dispose: () => {} };
  const controller = new PreviewController(editor, handle);
  lastActive = controller;
  // Editor context menu: Show/Hide Image Previews (the one that applies) and
  // the preview at the cursor. Keyboard users also have the palette commands.
  const on = editor.createContextKey<boolean>("expositionImagePreviewsOn", controller.enabled);
  controller.onEnabled = (enabled) => on.set(enabled);
  const actions = [
    editor.addAction({
      id: "exposition.imagePreviews.hide",
      label: "Hide Image Previews",
      precondition: "expositionImagePreviewsOn",
      contextMenuGroupId: "9_imagePreviews",
      contextMenuOrder: 1,
      run: () => controller.setDocumentPreviews(false),
    }),
    editor.addAction({
      id: "exposition.imagePreviews.show",
      label: "Show Image Previews",
      precondition: "!expositionImagePreviewsOn",
      contextMenuGroupId: "9_imagePreviews",
      contextMenuOrder: 1,
      run: () => controller.setDocumentPreviews(true),
    }),
    editor.addAction({
      id: "exposition.imagePreviews.toggleAtCursor",
      label: "Show or Hide Image Preview at Cursor",
      precondition: "expositionImagePreviewsOn",
      contextMenuGroupId: "9_imagePreviews",
      contextMenuOrder: 2,
      run: () => void controller.toggleAtCursor(),
    }),
  ];
  return {
    dispose: () => {
      actions.forEach((a) => a.dispose());
      controller.dispose();
    },
  };
}
