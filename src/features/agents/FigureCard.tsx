// A figure a tool call produced (native `run_python`), shown inline beside
// that call. Integration: the transcript renders
// `item.figures?.map((figure) => <FigureCard task={taskId} figure={figure} />)`
// for a `toolCall` item.
//
// Security: the reference comes from the application's record of the call
// (never from model text or a path), and the bytes come only from
// `agents_figure`, which serves artifacts of the task's own calls through the
// research artifact store (hash and size verified). Bytes are fetched once
// the card is visible, refused above FIGURE_MAX_BYTES, wrapped in a blob URL
// with the recorded media type, and rendered only as `<img>` — an SVG too, so
// its scripts, event handlers, and external references never run or load
// (CSP `img-src 'self' data: blob:` as well). The URL is revoked on unmount.
//
// Showing a figure is not keeping, promoting, or verifying it: the caption
// is agent text. "Open in Research" reveals the run (Research run
// inspector); "Insert or update in document…" is Research's own promotion
// flow (`PromotionControls`: plan review, document revision and artifact
// digest checks, explicit approval), offered when Research lists the run.
//
// Frameless, like a Research figure: the image keeps Research's figure
// treatment (`exp-research__figure`, the image's 1 px border only).
import { formatBytes } from "../../lib/format";
import { FlaskConical, ImageOff, Maximize2, RotateCcw } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { FigureRef } from "../../bindings/FigureRef";
import type { TaskId } from "../../bindings/TaskId";
import { commands, errorMessage } from "../../lib/ipc";
import { Button, Dialog, Spinner, StatusText } from "../../ui/components";
import { PromotionControls } from "../research/Promotion";
import { revealInResearch } from "../research/space";
import { useRun } from "../research/store";
import "../research/research.css";
import "./agents.css";

/** Figures larger than this are not decoded in the conversation. */
export const FIGURE_MAX_BYTES = 20 * 1024 * 1024;

/** Image types a figure may have (the run manifest's figure kinds). */
export const FIGURE_MEDIA_TYPES: ReadonlySet<string> = new Set(["image/png", "image/jpeg", "image/gif", "image/svg+xml"]);

export const FIGURE_NOTE = "Showing a figure does not add it to the document.";

type Load =
  | { kind: "waiting" } // not visible yet
  | { kind: "loading" }
  | { kind: "ready"; url: string }
  | { kind: "unavailable"; text: string }
  | { kind: "error"; text: string };

/** "run-4f2a9c1e…" → "4f2a9c1" (the full id is the title). */
export function shortRunId(runId: string): string {
  return runId.replace(/^run-/, "").slice(0, 7);
}

/** The unavailable reason as a sentence: "Figure no longer available — purged from Storage & History". */
export function unavailableText(reason: string): string {
  const text = reason.trim();
  if (!text) return "Figure no longer available";
  if (/^figure\b/i.test(text)) return text;
  return `Figure no longer available — ${/^[A-Z][a-z]/.test(text) ? text.charAt(0).toLowerCase() + text.slice(1) : text}`;
}

function codeOf(error: unknown): string | null {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string" ? error.code : null;
}

/** Size of the returned bytes (an ArrayBuffer or a view; -1 for anything else). */
function byteLength(data: unknown): number {
  if (ArrayBuffer.isView(data)) return data.byteLength;
  // `instanceof` fails across realms; the tag does not.
  if (Object.prototype.toString.call(data) === "[object ArrayBuffer]") return (data as ArrayBuffer).byteLength;
  return -1;
}

/** Whether the element is on screen (immediately true without IntersectionObserver). */
function useVisible<T extends Element>() {
  const ref = useRef<T>(null);
  const [visible, setVisible] = useState(() => typeof IntersectionObserver === "undefined");
  useEffect(() => {
    const el = ref.current;
    if (visible || !el) return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        setVisible(true);
        observer.disconnect();
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [visible]);
  return [ref, visible] as const;
}

export function FigureCard({ task, figure }: { task: TaskId; figure: FigureRef }) {
  const [ref, visible] = useVisible<HTMLElement>();
  const [load, setLoad] = useState<Load>({ kind: "waiting" });
  const [attempt, setAttempt] = useState(0);
  const [expanded, setExpanded] = useState(false);
  const supported = FIGURE_MEDIA_TYPES.has(figure.mediaType.toLowerCase());
  // Research's record of the run: "Insert" reuses its promotion flow.
  const run = useRun(figure.runId);
  const artifact = run?.outputs.find((a) => a.id === figure.artifactId);

  useEffect(() => {
    if (!supported) {
      setLoad({ kind: "unavailable", text: `This figure's type (${figure.mediaType}) is not shown here; open it in Research.` });
      return;
    }
    if (!visible) return;
    let live = true;
    let url: string | null = null;
    setLoad({ kind: "loading" });
    commands.agentsFigure(task, figure.runId, figure.artifactId).then(
      (bytes) => {
        if (!live) return;
        const size = byteLength(bytes);
        if (size < 0) {
          setLoad({ kind: "error", text: "The figure's data could not be read." });
        } else if (size > FIGURE_MAX_BYTES) {
          setLoad({
            kind: "unavailable",
            text: `Figure too large to show here (${formatBytes(size)}; the limit is ${formatBytes(FIGURE_MAX_BYTES)}). Open it in Research.`,
          });
        } else {
          url = URL.createObjectURL(new Blob([bytes as BlobPart], { type: figure.mediaType }));
          setLoad({ kind: "ready", url });
        }
      },
      (e: unknown) => {
        if (!live) return;
        if (codeOf(e) === "agents.figureUnavailable") setLoad({ kind: "unavailable", text: unavailableText(errorMessage(e)) });
        else setLoad({ kind: "error", text: `Could not load the figure: ${errorMessage(e)}` });
      },
    );
    return () => {
      live = false;
      if (url) URL.revokeObjectURL(url);
    };
  }, [task, figure.runId, figure.artifactId, figure.mediaType, supported, visible, attempt]);

  const alt = figure.caption?.trim() || figure.name;
  const provenance = [`Run ${shortRunId(figure.runId)}`, "Python"].join(" · ");

  return (
    <figure
      ref={ref}
      className="exp-research__figure"
      data-testid="agents-figure"
      data-state={load.kind}
      data-run={figure.runId}
      data-artifact={figure.artifactId}
    >
      {(load.kind === "waiting" || load.kind === "loading") && <Spinner size="sm" aria-label={`Loading ${figure.name}`} />}
      {load.kind === "ready" && (
        <img
          src={load.url}
          alt={alt}
          decoding="async"
          draggable={false}
          onError={() => setLoad({ kind: "error", text: "The figure could not be displayed (the image data is not valid)." })}
        />
      )}
      {load.kind === "unavailable" && (
        <div className="agents-row ui-meta" data-testid="agents-figure-unavailable">
          <ImageOff size={14} aria-hidden />
          <span>{load.text}</span>
        </div>
      )}
      {load.kind === "error" && (
        <div className="agents-row" role="alert" data-testid="agents-figure-error">
          <StatusText status="failed" wrap label={load.text} />
          <Button size="sm" variant="ghost" icon={RotateCcw} onPress={() => setAttempt((n) => n + 1)}>
            Retry
          </Button>
        </div>
      )}
      <figcaption className="agents-row">
        <code className="ui-code">{figure.name}</code>
        <span className="ui-meta" title={figure.runId}>
          {provenance}
        </span>
        {figure.caption && <span className="ui-meta">{figure.caption}</span>}
      </figcaption>
      {load.kind === "ready" && (
        <>
          <div className="agents-row">
            <Button size="sm" variant="ghost" icon={Maximize2} onPress={() => setExpanded(true)}>
              Expand
            </Button>
            <Button
              size="sm"
              variant="ghost"
              icon={FlaskConical}
              onPress={() => revealInResearch({ kind: "run", runId: figure.runId })}
            >
              Open in Research
            </Button>
          </div>
          {run && artifact && artifact.kind === "figure" && <PromotionControls run={run} artifact={artifact} />}
          <div className="ui-meta">{FIGURE_NOTE}</div>
          <Dialog
            title={figure.name}
            size="lg"
            body="content"
            isOpen={expanded}
            onOpenChange={setExpanded}
            footer={
              <Button variant="secondary" slot="close">
                Close
              </Button>
            }
          >
            <figure className="exp-research__figure agents-figure-expanded">
              <img src={load.url} alt={alt} draggable={false} />
              <figcaption className="ui-meta">
                {provenance}
                {figure.caption ? ` · ${figure.caption}` : ""}
              </figcaption>
            </figure>
          </Dialog>
        </>
      )}
    </figure>
  );
}
