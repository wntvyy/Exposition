// Read-only editor tab for project PDF files: pages stacked in a scrolling
// view, page navigation, fit width / actual size / zoom, panning by dragging
// or scrolling. Viewing never writes the file, never opens a document, starts
// no build, and creates nothing in the project graph. A PDF tab has no source
// relationship: no page editing, no equation editing, no manuscript sync.
// Pages are pictures rasterized in Rust (see pdf.ts): nothing in the PDF is
// executed or followed, and links are not clickable.
import { ChevronDown, ChevronUp, FileWarning, FileX, Lock, Maximize, ShieldAlert, ZoomIn, ZoomOut } from "lucide-react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";

import type { EditorProps } from "../../app/registry";
import { Banner, Button, ErrorState, IconButton, LoadingState, ToggleButton, Toolbar, UnavailableState } from "../../ui/components";
import { formatBytes } from "../../lib/format";
import {
  currentPage,
  fitWidthZoom,
  layoutPages,
  mountedPages,
  nextPageIndex,
  pageCountLabel,
  pageScrollTop,
  PDF_MAX_ZOOM,
  PDF_MIN_ZOOM,
  PdfLoadError,
  previousPageIndex,
  renderScale,
  stepPdfZoom,
  type PageHandle,
  type PdfPages,
} from "./pdf";
import { useProjectPdf } from "./useProjectPdf";
import "./media.css";

/** Wait this long after a zoom change before asking for sharper page images. */
const RESCALE_DELAY_MS = 150;

function ProblemState({ path, error, retry }: { path: string; error: PdfLoadError; retry: () => void }) {
  const common = { "data-testid": "pdf-viewer-state", "data-problem": error.problem, className: "xp-image-viewer__state" } as const;
  switch (error.problem) {
    case "missing":
      return (
        <div {...common}>
          <UnavailableState icon={FileX} title="PDF not found" description={`${path} was moved, renamed, or deleted.`} />
        </div>
      );
    case "blocked":
      return (
        <div {...common}>
          <UnavailableState icon={ShieldAlert} title="PDF outside the project" description={error.message} />
        </div>
      );
    case "tooLarge":
      return (
        <div {...common}>
          <UnavailableState icon={FileWarning} title="PDF too large to show" description={error.message} />
        </div>
      );
    case "encrypted":
      return (
        <div {...common}>
          <UnavailableState icon={Lock} title="Can't show an encrypted PDF" description={error.message} />
        </div>
      );
    case "empty":
      return (
        <div {...common}>
          <UnavailableState icon={FileWarning} title="No pages to show" description={error.message} />
        </div>
      );
    case "damaged":
    case "notPdf":
      return (
        <div {...common}>
          <UnavailableState icon={FileWarning} title="Can't show this PDF" description={error.message} />
        </div>
      );
    default:
      return (
        <div {...common}>
          <ErrorState
            title="Could not load this PDF"
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

/** One page: its frame at the laid-out size and, once rendered, its picture. */
function PdfPage({
  pages,
  index,
  scale,
  top,
  left,
  width,
  height,
}: {
  pages: PdfPages;
  index: number;
  scale: number;
  top: number;
  left: number;
  width: number;
  height: number;
}) {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const shown = useRef<PageHandle | null>(null);

  useEffect(() => {
    let live = true;
    let handle: PageHandle | null = null;
    // The first picture at once; a sharper one after the zoom has settled.
    const timer = setTimeout(
      () => {
        handle = pages.acquire(index, scale);
        const mine = handle;
        mine.promise.then(
          (next) => {
            if (!live) return;
            if (shown.current !== mine) shown.current?.release();
            shown.current = mine;
            setFailed(null);
            setUrl(next);
          },
          (error: unknown) => {
            if (!live) return;
            // A dropped request is not a result: the view is being replaced.
            if (error instanceof PdfLoadError && error.superseded) return;
            setFailed(error instanceof Error ? error.message : String(error));
          },
        );
      },
      shown.current ? RESCALE_DELAY_MS : 0,
    );
    return () => {
      live = false;
      clearTimeout(timer);
      if (handle && shown.current !== handle) handle.release();
    };
  }, [pages, index, scale]);

  useEffect(
    () => () => {
      shown.current?.release();
      shown.current = null;
    },
    [],
  );

  return (
    <div
      className="xp-pdf-viewer__page"
      data-testid="pdf-page"
      data-page={index + 1}
      data-state={url ? "ready" : failed ? "failed" : "loading"}
      style={{ top, left, width, height }}
    >
      {url ? (
        <img src={url} alt={`Page ${index + 1}`} draggable={false} width={width} height={height} />
      ) : failed ? (
        <span className="xp-pdf-viewer__page-note ui-meta" role="status">
          {failed}
        </span>
      ) : null}
    </div>
  );
}

export function PdfViewer({ resource, active }: EditorProps) {
  const path = resource.path ?? null;
  const loaded = useProjectPdf(path);
  const stage = useRef<HTMLDivElement>(null);
  const [fit, setFit] = useState(true);
  const [zoom, setZoom] = useState(1);
  const [stageSize, setStageSize] = useState({ width: 0, height: 0 });
  const [scrollTop, setScrollTop] = useState(0);
  const drag = useRef<{ x: number; y: number; left: number; top: number; id: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  // Never the pages of another file (the tab was re-keyed to a renamed file).
  const ready = loaded.status === "ready" && loaded.pdf.path === path ? loaded.pdf : null;
  const info = ready?.info ?? null;
  const widest = useMemo(() => (info ? Math.max(...info.pages.map((p) => p.width)) : 0), [info]);
  const scale = fit ? fitWidthZoom(widest, stageSize.width) : zoom;
  const layout = useMemo(() => layoutPages(info?.pages ?? [], scale), [info, scale]);
  const pageCount = info?.pages.length ?? 0;
  const page = currentPage(layout, scrollTop, stageSize.height);
  const { first, last } = mountedPages(layout, scrollTop, stageSize.height);
  const imageScale = renderScale(scale, typeof window === "undefined" ? 1 : window.devicePixelRatio);

  useLayoutEffect(() => {
    const el = stage.current;
    if (!el) return;
    const measure = () => setStageSize({ width: el.clientWidth, height: el.clientHeight });
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [ready !== null]);

  useEffect(() => {
    if (active && stage.current && ready) stage.current.focus({ preventScroll: true });
  }, [active, ready !== null]);

  // A zoom change keeps the same part of the document in view.
  const position = useRef(0);
  useLayoutEffect(() => {
    const el = stage.current;
    if (!el) return;
    el.scrollTop = position.current * layout.height;
    setScrollTop(el.scrollTop);
  }, [layout.height]);
  const onScroll = () => {
    const el = stage.current;
    if (!el) return;
    position.current = layout.height > 0 ? el.scrollTop / layout.height : 0;
    setScrollTop(el.scrollTop);
  };

  const zoomTo = useCallback((next: number) => {
    setFit(false);
    setZoom(Math.min(PDF_MAX_ZOOM, Math.max(PDF_MIN_ZOOM, next)));
  }, []);
  const zoomIn = () => zoomTo(stepPdfZoom(scale, 1));
  const zoomOut = () => zoomTo(stepPdfZoom(scale, -1));
  const actualSize = () => zoomTo(1);

  const goToPage = (index: number) => {
    const el = stage.current;
    if (!el) return;
    el.scrollTop = pageScrollTop(layout, index);
    onScroll();
  };
  const previousPage = () => goToPage(previousPageIndex(layout, stage.current?.scrollTop ?? scrollTop, stageSize.height));
  const nextPage = () => goToPage(nextPageIndex(layout, stage.current?.scrollTop ?? scrollTop));

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.key === "+" || event.key === "=") zoomIn();
    else if (event.key === "-") zoomOut();
    else if (event.key === "0") actualSize();
    else if (event.key === "!" || (event.shiftKey && event.code === "Digit1")) setFit(true);
    else return;
    event.preventDefault();
  };

  // Ctrl+wheel zooms; a plain wheel scrolls. Native, non-passive listener:
  // React's wheel listener cannot prevent the default.
  const wheel = useRef<(event: globalThis.WheelEvent) => void>(() => {});
  wheel.current = (event) => {
    if (!event.ctrlKey) return;
    event.preventDefault();
    zoomTo(scale * (event.deltaY < 0 ? 1.1 : 1 / 1.1));
  };
  useEffect(() => {
    const el = stage.current;
    if (!el) return;
    const listener = (event: globalThis.WheelEvent) => wheel.current(event);
    el.addEventListener("wheel", listener, { passive: false });
    return () => el.removeEventListener("wheel", listener);
  }, [ready !== null]);

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    const el = stage.current;
    if (!el || event.button !== 0) return;
    if (el.scrollWidth <= el.clientWidth && el.scrollHeight <= el.clientHeight) return;
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

  if (!path) return <ErrorState title="No PDF to show" />;
  const name = path.split("/").pop() ?? path;
  const facts = info ? [pageCountLabel(info.pageCount), "PDF", formatBytes(info.bytes)].join(" · ") : null;
  const truncated = info !== null && info.pageCount > info.pages.length;
  const contentWidth = Math.max(layout.width, stageSize.width);
  const mounted: number[] = [];
  // No page is requested before the stage is measured (fit width needs its size).
  if (stageSize.width > 0) for (let i = first; i <= last; i++) mounted.push(i);

  return (
    <div
      className="xp-image-viewer xp-pdf-viewer"
      data-testid="pdf-viewer"
      data-path={path}
      data-status={loaded.status === "error" ? loaded.error.problem : ready ? "ready" : "loading"}
      data-fit={fit ? "true" : "false"}
      data-zoom={Math.round(scale * 1000) / 1000}
      data-pages={ready ? pageCount : undefined}
      data-page={ready ? page + 1 : undefined}
      data-view={ready ? ready.info.token : undefined}
    >
      <Toolbar aria-label="PDF view" className="xp-image-viewer__toolbar">
        <IconButton aria-label="Previous page" icon={ChevronUp} size="sm" isDisabled={!ready || scrollTop <= 0} onPress={previousPage} />
        <span className="xp-pdf-viewer__pages" data-testid="pdf-page-indicator" aria-live="polite">
          {ready ? `Page ${page + 1} of ${pageCount}` : ""}
        </span>
        <IconButton aria-label="Next page" icon={ChevronDown} size="sm" isDisabled={!ready || page >= pageCount - 1} onPress={nextPage} />
        <ToggleButton size="sm" isSelected={fit} onChange={() => setFit(true)} isDisabled={!ready} icon={Maximize}>
          Fit width
        </ToggleButton>
        <Button size="sm" variant="ghost" onPress={actualSize} isDisabled={!ready}>
          Actual size
        </Button>
        <IconButton aria-label="Zoom out" icon={ZoomOut} size="sm" isDisabled={!ready || scale <= PDF_MIN_ZOOM} onPress={zoomOut} />
        <span className="xp-image-viewer__zoom" data-testid="pdf-zoom" aria-live="polite">
          {ready ? `${Math.round(scale * 100)}%` : ""}
        </span>
        <IconButton aria-label="Zoom in" icon={ZoomIn} size="sm" isDisabled={!ready || scale >= PDF_MAX_ZOOM} onPress={zoomIn} />
        {facts && (
          <span className="xp-image-viewer__facts ui-meta" data-testid="pdf-facts">
            {facts}
          </span>
        )}
      </Toolbar>
      {truncated && info && (
        <Banner tone="info">
          Showing the first {info.pages.length.toLocaleString("en-US")} of {info.pageCount.toLocaleString("en-US")} pages.
        </Banner>
      )}
      {loaded.status === "error" ? (
        <ProblemState path={path} error={loaded.error} retry={loaded.retry} />
      ) : !ready ? (
        <div className="xp-image-viewer__state">
          <LoadingState title="Loading PDF…" />
        </div>
      ) : (
        <div
          ref={stage}
          className="xp-pdf-viewer__stage"
          data-dragging={dragging || undefined}
          tabIndex={0}
          role="group"
          aria-label={`${name}, page ${page + 1} of ${pageCount}, ${Math.round(scale * 100)}%`}
          aria-roledescription="PDF view"
          onScroll={onScroll}
          onKeyDown={onKeyDown}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
        >
          <div className="xp-pdf-viewer__pages-area" style={{ width: contentWidth, height: layout.height }}>
            {mounted.map((i) => (
              <PdfPage
                key={`${ready.info.token}:${i}`}
                pages={ready.pages}
                index={i}
                scale={imageScale}
                top={layout.tops[i]!}
                left={Math.round((contentWidth - layout.widths[i]!) / 2)}
                width={layout.widths[i]!}
                height={layout.heights[i]!}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
