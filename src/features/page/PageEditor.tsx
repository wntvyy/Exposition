// Exact-page editor: the compiler's actual pages, edited in place.
//
// Pages are the build's SVG renderings shown as images (no active content).
// A hidden textarea at the caret is the keyboard/IME/clipboard target; the
// caret, selection, and composition are drawn over the pages from the page
// map's geometry. All source changes go through page transactions.
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { CSSProperties, KeyboardEvent, MouseEvent as ReactMouseEvent } from "react";
import { FileDown, Minus, Plus } from "lucide-react";

import type { EditorProps } from "../../app/registry";
import type { Motion } from "../../bindings/Motion";
import type { PageRect } from "../../bindings/PageRect";
import { errorMessage } from "../../lib/ipc";
import { summarize, useTypesetting } from "../typesetting/store";
import { PageSession } from "./session";
import "./page.css";

/** CSS pixels per typographic point at 100 % zoom. */
const PX_PER_PT = 96 / 72;

const MOTIONS: Record<string, Motion> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  Home: "lineStart",
  End: "lineEnd",
};

function rectStyle(rect: { x: number; y: number; width: number; height: number }, scale: number): CSSProperties {
  return { left: rect.x * scale, top: rect.y * scale, width: rect.width * scale, height: rect.height * scale };
}

export function PageEditor({ resource, active }: EditorProps) {
  const path = resource.path ?? "";
  const session = useMemo(() => new PageSession(path), [path]);
  const model = useSyncExternalStore(session.subscribe, session.getModel);
  const [zoom, setZoom] = useState(1.25);
  const [focused, setFocused] = useState(false);
  const input = useRef<HTMLTextAreaElement>(null);
  const caretRef = useRef<HTMLDivElement>(null);
  const composing = useRef(false);
  const dragging = useRef<{ page: number; element: HTMLElement } | null>(null);
  const scale = zoom * PX_PER_PT;
  const buildState = useTypesetting((s) => s.entries[path]);
  const reveal = useTypesetting((s) => s.reveal);

  useEffect(() => {
    void session.attach();
    return () => session.detach();
  }, [session]);

  useEffect(() => {
    if (reveal && reveal.path === path) void session.reveal(reveal.offset);
  }, [reveal, path, session]);

  useEffect(() => {
    if (active) input.current?.focus({ preventScroll: true });
  }, [active, model.phase]);

  // Keep the caret visible.
  useEffect(() => {
    caretRef.current?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [model.caret?.page, model.caret?.x, model.caret?.top]);

  const pointFor = (event: { clientX: number; clientY: number }, element: HTMLElement) => {
    const box = element.getBoundingClientRect();
    return { x: (event.clientX - box.left) / scale, y: (event.clientY - box.top) / scale };
  };

  const onPageMouseDown = (page: number) => (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    const element = event.currentTarget;
    const { x, y } = pointFor(event, element);
    dragging.current = { page, element };
    input.current?.focus({ preventScroll: true });
    void session.click(page, x, y, event.shiftKey);
  };

  useEffect(() => {
    let frame = 0;
    const move = (event: MouseEvent) => {
      const drag = dragging.current;
      if (!drag || frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        const { x, y } = pointFor(event, drag.element);
        void session.click(drag.page, x, y, true);
      });
    };
    const up = () => {
      dragging.current = null;
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      if (frame) cancelAnimationFrame(frame);
    };
  });

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (composing.current || event.nativeEvent.isComposing) return;
    const mod = event.ctrlKey || event.metaKey;
    const key = event.key;
    if (mod && (key === "z" || key === "Z")) {
      event.preventDefault();
      void session.undo(event.shiftKey);
      return;
    }
    if (mod && (key === "y" || key === "Y")) {
      event.preventDefault();
      void session.undo(true);
      return;
    }
    if (mod && (key === "a" || key === "A")) {
      event.preventDefault();
      void session.selectAll();
      return;
    }
    let motion = MOTIONS[key];
    if (motion && mod) {
      motion =
        key === "ArrowLeft"
          ? "wordLeft"
          : key === "ArrowRight"
            ? "wordRight"
            : key === "Home"
              ? "documentStart"
              : key === "End"
                ? "documentEnd"
                : motion;
    }
    if (motion) {
      event.preventDefault();
      void session.move(motion, event.shiftKey);
      return;
    }
    if (key === "Backspace" || key === "Delete") {
      event.preventDefault();
      void session.edit({ kind: key === "Backspace" ? "deleteBackward" : "deleteForward" });
      return;
    }
    if (key === "Enter") {
      event.preventDefault();
      void session.edit({ kind: "paragraphBreak" });
    }
  };

  const flushInput = () => {
    const element = input.current;
    if (!element || composing.current) return;
    const text = element.value;
    element.value = "";
    if (text) void session.edit({ kind: "insert", text });
  };

  const caret = model.caret;
  const caretStyle: CSSProperties | undefined = caret
    ? { left: caret.x * scale, top: caret.top * scale, height: Math.max(caret.bottom - caret.top, 4) * scale }
    : undefined;
  const status = summarize(buildState);
  const byPage = (rects: PageRect[], page: number) => rects.filter((r) => r.page === page);

  if (model.phase === "missing" || model.phase === "error") {
    return (
      <div className="page-editor page-editor--empty" role="alert">
        {model.error ?? "Document unavailable"}
      </div>
    );
  }

  return (
    <div
      className={`page-editor${focused ? " page-editor--focused" : ""}${model.stale ? " page-editor--stale" : ""}`}
      data-testid="page-editor"
      data-build={model.buildId ?? ""}
      data-stale={model.stale ? "true" : "false"}
      data-failed={model.failed ? "true" : "false"}
    >
      <div className="page-editor__toolbar" role="toolbar" aria-label="Page view">
        <span className="page-editor__title">{resource.title}</span>
        <span className={`page-editor__status page-editor__status--${status}`} data-testid="page-build-status">
          {status === "building"
            ? "Building…"
            : model.failed
              ? `${model.errorCount} error${model.errorCount === 1 ? "" : "s"} — showing last good pages`
              : status === "succeeded"
                ? model.latencyMs !== null
                  ? `Up to date · ${Math.round(model.latencyMs)} ms`
                  : "Up to date"
                : ""}
        </span>
        {caret && (
          <span className="page-editor__mode" data-testid="page-mode">
            {caret.mode === "math" ? "Math" : "Text"}
          </span>
        )}
        <button
          type="button"
          aria-label="Zoom out"
          onClick={() => setZoom((z) => Math.max(0.5, Math.round((z - 0.25) * 100) / 100))}
        >
          <Minus size={14} aria-hidden />
        </button>
        <span className="page-editor__zoom">{Math.round(zoom * 100)}%</span>
        <button
          type="button"
          aria-label="Zoom in"
          onClick={() => setZoom((z) => Math.min(4, Math.round((z + 0.25) * 100) / 100))}
        >
          <Plus size={14} aria-hidden />
        </button>
        <button
          type="button"
          aria-label="Export PDF"
          disabled={!model.buildId}
          onClick={() => {
            session.exportPdf().then(
              (file) => session.showMessage({ tone: "info", text: `Exported ${file}` }),
              (error: unknown) => session.showMessage({ tone: "warning", text: errorMessage(error) }),
            );
          }}
        >
          <FileDown size={14} aria-hidden /> PDF
        </button>
      </div>

      <div className="page-editor__scroller" data-testid="page-scroller">
        {model.pages.map((page) => {
          const width = page.widthPt * scale;
          const height = page.heightPt * scale;
          return (
            <div
              key={page.index}
              className="page-editor__page"
              data-testid="page"
              data-page={page.index}
              data-render-ref={page.renderRef}
              data-width-pt={page.widthPt}
              style={{ width, height }}
              onMouseDown={onPageMouseDown(page.index)}
            >
              {page.url && (
                <img src={page.url} width={width} height={height} alt={`Page ${page.index + 1}`} draggable={false} />
              )}
              {caret?.page === page.index &&
                caret.equation
                  .filter((r) => r.page === page.index)
                  .map((r, i) => <div key={`eq${i}`} className="page-editor__equation" style={rectStyle(r, scale)} />)}
              {byPage(model.selection, page.index).map((r, i) => (
                <div key={`s${i}`} className="page-editor__selection" style={rectStyle(r, scale)} />
              ))}
              {caret?.page === page.index && (
                <div
                  ref={caretRef}
                  className={`page-editor__caret${model.hasSelection ? " page-editor__caret--selecting" : ""}`}
                  data-testid="page-caret"
                  data-offset={session.caretPosition()?.offset ?? ""}
                  style={caretStyle}
                >
                  {model.composition && <span className="page-editor__composition">{model.composition}</span>}
                </div>
              )}
              {caret?.page === page.index && (
                <textarea
                  ref={input}
                  className="page-editor__input"
                  style={{ left: caret.x * scale, top: caret.top * scale }}
                  aria-label={`Page editor for ${resource.title}`}
                  aria-roledescription="page editor"
                  aria-multiline="true"
                  autoCapitalize="off"
                  autoComplete="off"
                  spellCheck={false}
                  data-testid="page-input"
                  onKeyDown={onKeyDown}
                  onInput={flushInput}
                  onFocus={() => setFocused(true)}
                  onBlur={() => setFocused(false)}
                  onCompositionStart={() => {
                    composing.current = true;
                  }}
                  onCompositionUpdate={(e) => session.setComposition(e.data)}
                  onCompositionEnd={(e) => {
                    composing.current = false;
                    session.setComposition("");
                    if (input.current) input.current.value = "";
                    if (e.data) void session.edit({ kind: "insert", text: e.data });
                  }}
                  onCopy={(e) => {
                    e.preventDefault();
                    if (model.selectionText) e.clipboardData.setData("text/plain", model.selectionText);
                  }}
                  onCut={(e) => {
                    e.preventDefault();
                    if (!model.selectionText) return;
                    e.clipboardData.setData("text/plain", model.selectionText);
                    void session.edit({ kind: "deleteBackward" });
                  }}
                  onPaste={(e) => {
                    e.preventDefault();
                    const text = e.clipboardData.getData("text/plain");
                    if (text) void session.edit({ kind: "insert", text });
                  }}
                />
              )}
            </div>
          );
        })}
        {model.pages.length === 0 && <div className="page-editor__placeholder">Building pages…</div>}
        {!caret && model.pages.length > 0 && (
          // Keyboard users can focus the editor before clicking: the first
          // arrow key places the caret at the document start.
          <textarea
            ref={input}
            className="page-editor__input"
            aria-label={`Page editor for ${resource.title}`}
            data-testid="page-input"
            onKeyDown={(e) => {
              if (e.key.startsWith("Arrow") || e.key === "Home") {
                e.preventDefault();
                void session.reveal(0).then(() => session.move("documentStart", false));
              }
            }}
            onFocus={() => setFocused(true)}
            onBlur={() => setFocused(false)}
          />
        )}
      </div>

      <div className="page-editor__message" role="status" aria-live="polite" data-testid="page-message">
        {model.message?.text ?? ""}
      </div>
      <div className="page-editor__sr-only" aria-live="polite" data-testid="page-line">
        {caret ? caret.lineText : ""}
      </div>
    </div>
  );
}
