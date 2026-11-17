// Read-only image editor tab for project images (PNG, JPEG, GIF, WebP, BMP,
// SVG, AVIF, ICO). Fit / actual size / zoom, panning by dragging or scrolling.
// Viewing never writes the file, never opens a document, and creates nothing
// in the project graph. Images are shown only through <img> from a blob: URL
// made by the shared loader (SVG stays inert; see loader.ts).
import { FileWarning, ImageOff, Maximize, ShieldAlert, ZoomIn, ZoomOut } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";

import type { EditorProps } from "../../app/registry";
import { Button, ErrorState, IconButton, LoadingState, ToggleButton, Toolbar, UnavailableState } from "../../ui/components";
import { FORMAT_LABELS, formatBytes, type ImageLoadError } from "./loader";
import { useProjectImage } from "./useProjectImage";
import "./media.css";

const ZOOM_STEPS = [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1, 1.25, 1.5, 2, 3, 4, 6, 8];
const MIN_ZOOM = ZOOM_STEPS[0]!;
const MAX_ZOOM = ZOOM_STEPS[ZOOM_STEPS.length - 1]!;

/** Next zoom step above (direction 1) or below (-1) `zoom`. */
export function stepZoom(zoom: number, direction: 1 | -1): number {
  if (direction > 0) return ZOOM_STEPS.find((z) => z > zoom + 1e-6) ?? MAX_ZOOM;
  return [...ZOOM_STEPS].reverse().find((z) => z < zoom - 1e-6) ?? MIN_ZOOM;
}

/** Scale that fits `width`×`height` into the stage (never enlarges). */
export function fitZoom(width: number, height: number, stageWidth: number, stageHeight: number): number {
  if (width <= 0 || height <= 0 || stageWidth <= 0 || stageHeight <= 0) return 1;
  return Math.max(MIN_ZOOM / 10, Math.min(1, stageWidth / width, stageHeight / height));
}

function ProblemState({ path, error, retry }: { path: string; error: ImageLoadError; retry: () => void }) {
  const name = path.split("/").pop() ?? path;
  const common = { "data-testid": "image-viewer-state", "data-problem": error.problem } as const;
  switch (error.problem) {
    case "missing":
      return (
        <div {...common} className="xp-image-viewer__state">
          <UnavailableState icon={ImageOff} title="Image not found" description={`${path} was moved, renamed, or deleted.`} />
        </div>
      );
    case "blocked":
      return (
        <div {...common} className="xp-image-viewer__state">
          <UnavailableState icon={ShieldAlert} title="Image outside the project" description={error.message} />
        </div>
      );
    case "tooLarge":
      return (
        <div {...common} className="xp-image-viewer__state">
          <UnavailableState icon={FileWarning} title="Image too large to show" description={error.message} />
        </div>
      );
    case "unsupported":
      return (
        <div {...common} className="xp-image-viewer__state">
          <UnavailableState
            icon={FileWarning}
            title="Can't show this image format"
            description={`${name}: shown formats are PNG, JPEG, GIF, WebP, BMP, SVG, AVIF, and ICO.`}
          />
        </div>
      );
    case "damaged":
      return (
        <div {...common} className="xp-image-viewer__state">
          <UnavailableState icon={FileWarning} title="Can't show this image" description={error.message} />
        </div>
      );
    default:
      return (
        <div {...common} className="xp-image-viewer__state">
          <ErrorState
            title="Could not load this image"
            description={error.message}
            actions={
              <Button variant="secondary" onPress={retry}>
                Retry
              </Button>
            }
          />
        </div>
      );
  }
}

export function ImageViewer({ resource, active }: EditorProps) {
  const path = resource.path ?? null;
  const image = useProjectImage(path, "full");
  const stage = useRef<HTMLDivElement>(null);
  const [fit, setFit] = useState(true);
  const [zoom, setZoom] = useState(1);
  const [stageSize, setStageSize] = useState({ width: 0, height: 0 });
  const drag = useRef<{ x: number; y: number; left: number; top: number; id: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  const ready = image.status === "ready" ? image.image : null;
  const fitted = ready ? fitZoom(ready.width, ready.height, stageSize.width, stageSize.height) : 1;
  const scale = fit ? fitted : zoom;

  useLayoutEffect(() => {
    const el = stage.current;
    if (!el) return;
    const measure = () => setStageSize({ width: el.clientWidth - 32, height: el.clientHeight - 32 });
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [ready !== null]);

  useEffect(() => {
    if (active && stage.current && ready) stage.current.focus({ preventScroll: true });
  }, [active, ready !== null]);

  /** Zoom to `next`, keeping the point under (clientX, clientY) — or the center — in place. */
  const zoomTo = useCallback(
    (next: number, at?: { clientX: number; clientY: number }) => {
      const el = stage.current;
      const clamped = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, next));
      if (el && ready) {
        const rect = el.getBoundingClientRect();
        const px = at ? at.clientX - rect.left : el.clientWidth / 2;
        const py = at ? at.clientY - rect.top : el.clientHeight / 2;
        const fx = (el.scrollLeft + px) / Math.max(1, el.scrollWidth);
        const fy = (el.scrollTop + py) / Math.max(1, el.scrollHeight);
        requestAnimationFrame(() => {
          el.scrollLeft = fx * el.scrollWidth - px;
          el.scrollTop = fy * el.scrollHeight - py;
        });
      }
      setFit(false);
      setZoom(clamped);
    },
    [ready],
  );

  const zoomIn = () => zoomTo(stepZoom(scale, 1));
  const zoomOut = () => zoomTo(stepZoom(scale, -1));
  const actualSize = () => zoomTo(1);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.key === "+" || event.key === "=") zoomIn();
    else if (event.key === "-") zoomOut();
    else if (event.key === "0") actualSize();
    else if (event.key === "!" || (event.shiftKey && event.code === "Digit1")) setFit(true);
    else return;
    event.preventDefault();
  };

  // Ctrl+wheel zooms at the pointer; a plain wheel scrolls (pans). Native,
  // non-passive listener: React's wheel listener cannot prevent the default.
  const wheel = useRef<(event: globalThis.WheelEvent) => void>(() => {});
  wheel.current = (event) => {
    if (!event.ctrlKey) return;
    event.preventDefault();
    zoomTo(scale * (event.deltaY < 0 ? 1.1 : 1 / 1.1), event);
  };
  useEffect(() => {
    const el = stage.current;
    if (!el) return;
    const listener = (event: globalThis.WheelEvent) => wheel.current(event);
    el.addEventListener("wheel", listener, { passive: false });
    return () => el.removeEventListener("wheel", listener);
  }, [ready !== null]);

  const pannable = () => {
    const el = stage.current;
    return Boolean(el && (el.scrollWidth > el.clientWidth || el.scrollHeight > el.clientHeight));
  };

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    const el = stage.current;
    if (!el || event.button !== 0 || !pannable()) return;
    drag.current = { x: event.clientX, y: event.clientY, left: el.scrollLeft, top: el.scrollTop, id: event.pointerId };
    el.setPointerCapture(event.pointerId);
    setDragging(true);
  };
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const el = stage.current;
    const d = drag.current;
    if (!el || !d || d.id !== event.pointerId) return;
    el.scrollLeft = d.left - (event.clientX - d.x);
    el.scrollTop = d.top - (event.clientY - d.y);
  };
  const endDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.id !== event.pointerId) return;
    drag.current = null;
    setDragging(false);
  };

  if (!path) return <ErrorState title="No image to show" />;
  const name = path.split("/").pop() ?? path;
  const facts = ready ? [`${ready.width} × ${ready.height}`, FORMAT_LABELS[ready.format], formatBytes(ready.bytes)].join(" · ") : null;

  return (
    <div
      className="xp-image-viewer"
      data-testid="image-viewer"
      data-path={path}
      data-status={image.status === "error" ? image.error.problem : image.status}
      data-fit={fit ? "true" : "false"}
      data-zoom={Math.round(scale * 1000) / 1000}
    >
      <Toolbar aria-label="Image view" className="xp-image-viewer__toolbar">
        <ToggleButton size="sm" isSelected={fit} onChange={() => setFit(true)} isDisabled={!ready} icon={Maximize}>
          Fit
        </ToggleButton>
        <Button size="sm" variant="ghost" onPress={actualSize} isDisabled={!ready}>
          Actual size
        </Button>
        <IconButton aria-label="Zoom out" icon={ZoomOut} size="sm" isDisabled={!ready || scale <= MIN_ZOOM} onPress={zoomOut} />
        <span className="xp-image-viewer__zoom" data-testid="image-zoom" aria-live="polite">
          {ready ? `${Math.round(scale * 100)}%` : ""}
        </span>
        <IconButton aria-label="Zoom in" icon={ZoomIn} size="sm" isDisabled={!ready || scale >= MAX_ZOOM} onPress={zoomIn} />
        {facts && (
          <span className="xp-image-viewer__facts ui-meta" data-testid="image-facts">
            {facts}
          </span>
        )}
      </Toolbar>
      {image.status === "error" ? (
        <ProblemState path={path} error={image.error} retry={image.retry} />
      ) : !ready ? (
        <div className="xp-image-viewer__state">
          <LoadingState title="Loading image…" />
        </div>
      ) : (
        <div
          ref={stage}
          className="xp-image-viewer__stage"
          data-dragging={dragging || undefined}
          data-pannable={!fit || undefined}
          tabIndex={0}
          role="group"
          aria-label={`${name}, ${Math.round(scale * 100)}%`}
          aria-roledescription="image view"
          onKeyDown={onKeyDown}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
        >
          <div className="xp-image-viewer__canvas">
            <img
              className="xp-image-viewer__image"
              data-testid="image-viewer-image"
              src={ready.url}
              alt={name}
              draggable={false}
              width={Math.max(1, Math.round(ready.width * scale))}
              height={Math.max(1, Math.round(ready.height * scale))}
              data-pixelated={scale >= 2 || undefined}
            />
          </div>
        </div>
      )}
    </div>
  );
}
