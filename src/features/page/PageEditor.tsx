// Exact-page editor: the compiler's actual pages, edited in place.
//
// Pages are the build's SVG renderings shown as images (no active content).
// One hidden textarea, kept at the caret, is the keyboard/IME/clipboard
// target; caret, selection, equation outline, and composition are drawn over
// the pages from the page map. All source changes are page transactions.
import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { CSSProperties, KeyboardEvent, MouseEvent as ReactMouseEvent } from "react";
import { FileDown, FileWarning, RefreshCw, ZoomIn, ZoomOut } from "lucide-react";

import { notify } from "../../app/notifications";
import type { EditorProps } from "../../app/registry";
import type { Motion } from "../../bindings/Motion";
import type { PageRect } from "../../bindings/PageRect";
import { commands, errorMessage } from "../../lib/ipc";
import {
  Banner,
  Button,
  ErrorState,
  IconButton,
  LoadingState,
  StatusPill,
  Toolbar,
  UnavailableState,
} from "../../ui/components";
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

const MOD_MOTIONS: Record<string, Motion> = {
  ArrowLeft: "wordLeft",
  ArrowRight: "wordRight",
  Home: "documentStart",
  End: "documentEnd",
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
  const pageRefs = useRef(new Map<number, HTMLDivElement>());
  const composing = useRef(false);
  const dragging = useRef<{ page: number; element: HTMLElement } | null>(null);
  const scale = zoom * PX_PER_PT;
  const buildState = useTypesetting((s) => s.entries[path]);
  const reveal = useTypesetting((s) => s.reveal);
  const caret = model.caret;

  useEffect(() => {
    void session.attach();
    return () => session.detach();
  }, [session]);

  useEffect(() => {
    if (reveal && reveal.path === path) void session.reveal(reveal.offset);
  }, [reveal, path, session]);

  useEffect(() => {
    if (active && model.pages.length > 0) input.current?.focus({ preventScroll: true });
  }, [active, model.pages.length]);

  // Keep the input at the caret (IME candidate windows open there) and the caret visible.
  useLayoutEffect(() => {
    const element = input.current;
    if (!element) return;
    const page = caret ? pageRefs.current.get(caret.page) : pageRefs.current.get(0);
    const left = (page?.offsetLeft ?? 0) + (caret ? caret.x * scale : 0);
    const top = (page?.offsetTop ?? 0) + (caret ? caret.top * scale : 0);
    element.style.left = `${left}px`;
    element.style.top = `${top}px`;
    element.style.height = `${caret ? Math.max(caret.bottom - caret.top, 4) * scale : 16}px`;
    caretRef.current?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [caret, scale, model.pages]);

  const pointFor = (event: { clientX: number; clientY: number }, element: HTMLElement) => {
    const box = element.getBoundingClientRect();
    return { x: (event.clientX - box.left) / scale, y: (event.clientY - box.top) / scale };
  };

  const onPageMouseDown = (page: number) => (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    input.current?.focus({ preventScroll: true });
    const element = event.currentTarget;
    const { x, y } = pointFor(event, element);
    dragging.current = { page, element };
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
    const lower = key.toLowerCase();
    if (mod && !event.altKey && (lower === "z" || lower === "y")) {
      event.preventDefault();
      void session.undo(lower === "y" || event.shiftKey);
      return;
    }
    if (mod && !event.altKey && lower === "a") {
      event.preventDefault();
      void session.selectAll();
      return;
    }
    const motion = (mod ? MOD_MOTIONS[key] : undefined) ?? MOTIONS[key];
    if (motion && !event.altKey) {
      event.preventDefault();
      void session.move(motion, event.shiftKey);
      return;
    }
    if (key === "Backspace" || key === "Delete") {
      event.preventDefault();
      void session.edit({ kind: key === "Backspace" ? "deleteBackward" : "deleteForward" });
      return;
    }
    if (key === "Enter" && !mod) {
      event.preventDefault();
      void session.edit({ kind: "paragraphBreak" });
      return;
    }
    if (key === "Escape" && model.hasSelection) {
      event.preventDefault();
      void session.collapse();
    }
  };

  const flushInput = () => {
    const element = input.current;
    if (!element || composing.current) return;
    const text = element.value;
    element.value = "";
    if (text) void session.edit({ kind: "insert", text });
  };

  const exportPdf = () => {
    session.exportPdf().then(
      (file) => notify({ tone: "success", title: "Exported PDF", body: file }),
      (error: unknown) => notify({ tone: "danger", title: "PDF export failed", body: errorMessage(error) }),
    );
  };

  const status = summarize(buildState);
  const byPage = (rects: PageRect[], page: number) => rects.filter((r) => r.page === page);
  const caretStyle: CSSProperties | undefined = caret
    ? { left: caret.x * scale, top: caret.top * scale, height: Math.max(caret.bottom - caret.top, 4) * scale }
    : undefined;
  const errors = `${model.errorCount} error${model.errorCount === 1 ? "" : "s"}`;

  if (model.phase === "missing" || model.phase === "error") {
    return (
      <ErrorState
        title={`Cannot show pages of ${resource.title}`}
        description={model.error ?? "The document is not available."}
      />
    );
  }

  const pill =
    status === "building" ? (
      <StatusPill status="running" label={`Building ${resource.title}`} size="sm" />
    ) : model.failed ? (
      <StatusPill status="failed" label="Build failed" detail={errors} size="sm" />
    ) : model.buildId ? (
      <StatusPill
        status="succeeded"
        label="Built"
        detail={model.latencyMs !== null ? `${Math.round(model.latencyMs)} ms` : undefined}
        size="sm"
      />
    ) : null;

  return (
    <div
      className={`page-editor${focused ? " page-editor--focused" : ""}${model.failed ? " page-editor--stale" : ""}`}
      data-testid="page-editor"
      data-build={model.buildId ?? ""}
      data-build-revision={model.buildRevision ?? ""}
      data-stale={model.stale ? "true" : "false"}
      data-failed={model.failed ? "true" : "false"}
    >
      <Toolbar aria-label="Page view" className="page-editor__toolbar">
        <span className="page-editor__status" data-testid="page-build-status">
          {pill}
        </span>
        {caret && (
          <span className="page-editor__mode" data-testid="page-mode">
            {caret.mode === "math" ? "Math" : "Text"}
          </span>
        )}
        <IconButton
          aria-label="Zoom out"
          icon={ZoomOut}
          size="sm"
          onPress={() => setZoom((z) => Math.max(0.5, Math.round((z - 0.25) * 100) / 100))}
        />
        <span className="page-editor__zoom">{Math.round(zoom * 100)}%</span>
        <IconButton
          aria-label="Zoom in"
          icon={ZoomIn}
          size="sm"
          onPress={() => setZoom((z) => Math.min(4, Math.round((z + 0.25) * 100) / 100))}
        />
        <IconButton aria-label="Export PDF" icon={FileDown} size="sm" isDisabled={!model.buildId} onPress={exportPdf} />
      </Toolbar>

      {model.failed && model.pages.length > 0 && (
        <Banner
          tone="stale"
          title="Showing pages from an earlier revision"
          actions={
            <Button variant="ghost" size="sm" icon={RefreshCw} onPress={() => void commands.typesettingBuild(path)}>
              Rebuild
            </Button>
          }
        >
          The document has {errors} (see Problems). Placing the caret and moving on these pages is disabled until it
          builds; typing and deleting continue at the caret.
        </Banner>
      )}

      <div className="page-editor__scroller" data-testid="page-scroller">
        {model.pages.map((page) => {
          const width = page.widthPt * scale;
          const height = page.heightPt * scale;
          return (
            <div
              key={page.index}
              ref={(el) => {
                if (el) pageRefs.current.set(page.index, el);
                else pageRefs.current.delete(page.index);
              }}
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
                  data-anchor={session.caretPosition()?.anchor ?? ""}
                  data-selection-rects={model.selection.length}
                  style={caretStyle}
                >
                  {model.composition && <span className="page-editor__composition">{model.composition}</span>}
                </div>
              )}
            </div>
          );
        })}
        {model.pages.length === 0 &&
          (model.untrustedRoot ? (
            <UnavailableState
              title="Workspace not trusted"
              description="Pages of this document are built by running its compiler, which runs with your account's permissions. Trust the workspace to build and edit its pages."
              actions={
                <Button variant="primary" size="sm" onPress={() => void session.trust()}>
                  Trust workspace…
                </Button>
              }
            />
          ) : model.failed ? (
            <ErrorState
              title="The document did not build"
              description={`${errors} — see Problems. Pages appear after the first successful build.`}
            />
          ) : (
            <LoadingState title="Building pages…" />
          ))}
        <textarea
          ref={input}
          className="page-editor__input"
          aria-label={`Page editor for ${resource.title}`}
          aria-roledescription="page editor"
          aria-multiline="true"
          aria-describedby={`page-line-${resource.key}`}
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
      </div>

      <div className="page-editor__message" role="status" aria-live="polite" data-testid="page-message">
        {model.message && (
          <>
            <FileWarning size={14} aria-hidden />
            {model.message.text}
          </>
        )}
      </div>
      <div id={`page-line-${resource.key}`} className="page-editor__sr-only" aria-live="polite" data-testid="page-line">
        {caret ? caret.lineText : ""}
      </div>
    </div>
  );
}
