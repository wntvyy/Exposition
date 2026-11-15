// The shared renderer for untrusted Markdown (agent responses; any module that
// shows model- or tool-written prose uses this, never its own renderer), and
// the response surface that frames one agent response.
//
// ---------------------------------------------------------------------------
// Security audit of the renderer configuration (A1, decision D1). Everything a
// response contains is untrusted. Rendering or opening a message never runs
// code, never navigates the window, and never makes a network request.
//
// Pipeline: react-markdown 10 (remark-parse → remark-rehype → React elements,
// no innerHTML) with exactly these plugins and options:
//  - remark-gfm 4: syntax only (tables, strikethrough, task lists, autolink
//    literals, footnotes). It adds no HTML and no URL schemes beyond
//    `http://` for `www.` literals.
//  - remark-math 6: syntax only; math reaches React as `code.math-inline` /
//    `pre > code.math-display` text and is typeset by MathTeX below.
//  - remarkLiteralHtml (this file): every raw HTML node (block or inline) is
//    turned into plain text before conversion, so `<script>`, `<img onerror>`,
//    `<iframe>`, `<style>`, comments, and entities-in-tags are shown as the
//    characters the model wrote (`Vec<T>` stays readable) and never become
//    elements. `skipHtml` is also set as a second barrier (any raw node that
//    survived would be dropped). No rehype-raw, no rehype plugins at all.
//  - allowedElements: only the elements Markdown itself produces; anything
//    else is unwrapped to its children (`unwrapDisallowed`).
//  - urlTransform: passes `a[href]` and `img[src]` through unchanged (the
//    components below classify them) and blanks every other URL attribute.
// Custom components:
//  - a: never rendered with `href` or `target`. An address that passes
//    `externalUrl` (absolute http/https with a host; no credentials,
//    whitespace, control, invisible or bidi characters; ≤ 2048 characters —
//    the same rule `app_open_external` re-checks in Rust) becomes a focusable
//    role="link" element shown with its address; click or Enter calls
//    preventDefault and opens it in the system browser through
//    `commands.appOpenExternal` (the only external-navigation route; the
//    window's navigation guard refuses everything else). `javascript:`,
//    `vbscript:`, `data:`, `file:`, `tauri:`, `ipc:`, `mailto:`, relative and
//    fragment links (in any spelling: case, entities, whitespace, control
//    characters) render as inert text with the address shown.
//  - img: never an <img>; a placeholder with the alt text and the address,
//    plus an Open button for http(s) only. Remote and data: images never load
//    (the CSP also limits img-src to the app's own origin, data: and blob:).
//  - pre/code: fence text rendered as React text; highlighting comes from
//    Monaco's tokenizer as data (MarkdownHighlight.ts), never an HTML string.
//    Copy copies the fence text exactly. A Run action exists only when the
//    host passes `runAction` (the transcript passes "Run in Research", a user
//    action into the research session); nothing runs on render.
//  - math: KaTeX via `katex.render` (DOM nodes, no HTML parsing) with
//    `trust: false` (no \href, \url, \includegraphics, \html*), `strict:
//    "ignore"`, `throwOnError: false`, `maxSize` 10em, `maxExpand` 500, no
//    shared macros, ≤ 20 000 characters; a formula KaTeX cannot parse is
//    shown as TeX source in code. KaTeX's fonts and CSS are bundled
//    (font-src 'self').
//  - table/input: scroll wrapper; task-list checkboxes are rendered disabled.
// The renderer has no IPC access besides the user-initiated link opening, and
// reads nothing from the project.
// ---------------------------------------------------------------------------
import "./Markdown.css";

import { ExternalLink, Image as ImageIcon } from "lucide-react";
import {
  createContext,
  memo,
  useContext,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
} from "react";
import ReactMarkdown, { type Components, type UrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";

import { commands } from "../../lib/ipc";
import { Button } from "./Button";
import { cx } from "./cx";
import { CodeBlock, CopyButton, type CodeRunAction } from "./MarkdownCode";
import { MathTeX } from "./MarkdownMath";
import { displayAddress, externalUrl, fenceLanguage, isOpenFence, isOpenMath, normalizeMathDelimiters } from "./MarkdownPolicy";
import { useOverflows } from "./MarkdownScroll";
import { useStateSize } from "./stateSize";

export type { CodeRunAction } from "./MarkdownCode";

// ---------------------------------------------------------------- tree helpers

interface MdNode {
  type: string;
  value?: string;
  children?: MdNode[];
  data?: Record<string, unknown>;
}

interface HastNode {
  type: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
  position?: { start: { offset?: number }; end: { offset?: number } };
}

const FLOW_PARENTS = new Set(["root", "blockquote", "listItem", "footnoteDefinition"]);

/** remark plugin: raw HTML becomes literal text (see the audit above). */
function remarkLiteralHtml() {
  const walk = (node: MdNode) => {
    const children = node.children;
    if (!children) return;
    for (let i = 0; i < children.length; i += 1) {
      const child = children[i]!;
      if (child.type === "html") {
        const text: MdNode = { type: "text", value: child.value ?? "" };
        children[i] = FLOW_PARENTS.has(node.type)
          ? { type: "paragraph", data: { hProperties: { className: ["ui-md-raw"] } }, children: [text] }
          : text;
      } else {
        walk(child);
      }
    }
  };
  return (tree: MdNode) => walk(tree);
}

function hastText(node: HastNode | undefined): string {
  if (!node) return "";
  if (node.type === "text") return node.value ?? "";
  return (node.children ?? []).map(hastText).join("");
}

function hasClass(node: HastNode | undefined, name: string): boolean {
  const className = node?.properties?.className;
  return Array.isArray(className) ? className.includes(name) : className === name;
}

// ---------------------------------------------------------------- context

interface RenderContext {
  /** The text the tree was parsed from (after delimiter normalization). */
  source: string;
  streaming: boolean;
  runAction?: CodeRunAction;
  openExternal: (url: string) => void;
}

const MarkdownContext = createContext<RenderContext>({
  source: "",
  streaming: false,
  openExternal: () => {},
});

function defaultOpenExternal(url: string): void {
  void commands.appOpenExternal(url).catch((e: unknown) => console.warn("Could not open the link", e));
}

// ---------------------------------------------------------------- components
// Module-level components (stable identities: a re-render while streaming
// keeps code highlighting and typeset math instead of remounting them).

function MdLink({ node, children }: { node?: unknown; children?: ReactNode }) {
  const { openExternal } = useContext(MarkdownContext);
  const element = node as HastNode | undefined;
  const raw = String(element?.properties?.href ?? "");
  const url = externalUrl(raw);
  const text = hastText(element);
  // An autolink shows its address already (`www.` literals gain `http://`); an
  // in-message fragment (footnote references and back-references) has none worth showing.
  const showsAddress = text === raw || `http://${text}` === raw || `mailto:${text}` === raw || raw === "" || raw.startsWith("#");
  const address = showsAddress ? null : (
    <span className="ui-md-link__address">{url ?? displayAddress(raw)}</span>
  );
  if (!url) {
    return (
      <span className="ui-md-link-inert" data-inert-link="">
        {children}
        {address && <> {address}</>}
      </span>
    );
  }
  const open = (event: MouseEvent | KeyboardEvent) => {
    event.preventDefault();
    event.stopPropagation();
    openExternal(url);
  };
  return (
    <>
      <a
        className="ui-md-link"
        role="link"
        tabIndex={0}
        data-external-link=""
        onClick={open}
        onAuxClick={(event) => event.preventDefault()}
        onKeyDown={(event) => {
          if (event.key === "Enter") open(event);
        }}
      >
        {children}
      </a>
      {address && <> {address}</>}
    </>
  );
}

function MdImage({ src, alt }: { src?: unknown; alt?: string }) {
  const { openExternal } = useContext(MarkdownContext);
  const raw = typeof src === "string" ? src : "";
  const url = externalUrl(raw);
  const name = alt?.trim() || "Image";
  return (
    <span className="ui-md-image" data-testid="md-image">
      <ImageIcon size={14} aria-hidden className="ui-md-image__icon" />
      <span className="ui-md-image__text">
        <span className="ui-md-image__alt">{name}</span>
        <span className="ui-md-image__note">
          Image not loaded{raw ? ": " : ""}
          {raw && <span className="ui-md-image__address">{displayAddress(raw)}</span>}
        </span>
      </span>
      {url && (
        <Button size="sm" variant="ghost" icon={ExternalLink} aria-label={`Open ${name} in the browser`} onPress={() => openExternal(url)}>
          Open
        </Button>
      )}
    </span>
  );
}

function MdPre({ node }: { node?: unknown }) {
  const { source, streaming, runAction } = useContext(MarkdownContext);
  const pre = node as HastNode | undefined;
  const code = pre?.children?.find((c) => c.type === "element" && c.tagName === "code");
  const start = pre?.position?.start.offset ?? 0;
  const end = pre?.position?.end.offset ?? source.length;
  if (hasClass(code, "math-display") || hasClass(code, "language-math")) {
    const tex = hastText(code).replace(/\n$/, "");
    const open = streaming && hasClass(code, "math-display") && isOpenMath(source, start, end);
    return <MathTeX tex={tex} display open={open} />;
  }
  // mdast-util-to-hast appends one newline to the fence text; Copy and Run use the text as written.
  const text = hastText(code).replace(/\n$/, "");
  const open = streaming && isOpenFence(source, start, end);
  return <CodeBlock code={text} language={fenceLanguage(code?.properties?.className)} open={open} runAction={runAction} />;
}

function MdCode({ node, children }: { node?: unknown; children?: ReactNode }) {
  const element = node as HastNode | undefined;
  if (hasClass(element, "math-inline")) return <MathTeX tex={hastText(element)} display={false} />;
  return <code className="ui-md-code-inline">{children}</code>;
}

function MdTable({ children }: { children?: ReactNode }) {
  const [ref, scrolls] = useOverflows<HTMLDivElement>(children);
  return (
    <div
      ref={ref}
      className="ui-md-table"
      tabIndex={scrolls ? 0 : undefined}
      role={scrolls ? "region" : undefined}
      aria-label={scrolls ? "Table" : undefined}
    >
      <table>{children}</table>
    </div>
  );
}

function MdInput({ type, checked }: { type?: string; checked?: boolean }) {
  if (type !== "checkbox") return null;
  return <input type="checkbox" className="ui-md-task" checked={Boolean(checked)} disabled readOnly />;
}

const COMPONENTS: Components = {
  a: MdLink,
  img: MdImage,
  pre: MdPre,
  code: MdCode,
  table: MdTable,
  input: MdInput,
};

/** Elements Markdown (CommonMark + GFM + math + footnotes) produces; anything else is unwrapped. */
const ALLOWED_ELEMENTS = [
  "p", "h1", "h2", "h3", "h4", "h5", "h6", "em", "strong", "del", "a", "img", "code", "pre", "blockquote",
  "ul", "ol", "li", "table", "thead", "tbody", "tr", "th", "td", "hr", "br", "input", "sup", "section",
];

const urlTransform: UrlTransform = (url, key, node) =>
  (node.tagName === "a" && key === "href") || (node.tagName === "img" && key === "src") ? url : "";

const REMARK_PLUGINS = [remarkGfm, [remarkMath, { singleDollarTextMath: true }], remarkLiteralHtml] as NonNullable<
  Parameters<typeof ReactMarkdown>[0]["remarkPlugins"]
>;

// ---------------------------------------------------------------- streaming

/**
 * While `active`, follow `value` at most once per animation frame (a burst of
 * streamed deltas re-parses once per frame); otherwise return it directly.
 */
export function useFrameThrottled<T>(value: T, active: boolean): T {
  const [shown, setShown] = useState(value);
  const latest = useRef(value);
  latest.current = value;
  const frame = useRef<number | null>(null);
  useEffect(() => {
    if (!active) return;
    if (frame.current !== null) return;
    const schedule = typeof requestAnimationFrame === "function" ? requestAnimationFrame : (cb: () => void) => setTimeout(cb, 16) as unknown as number;
    frame.current = schedule(() => {
      frame.current = null;
      setShown(latest.current);
    });
  }, [value, active]);
  useEffect(
    () => () => {
      if (frame.current !== null && typeof cancelAnimationFrame === "function") cancelAnimationFrame(frame.current);
    },
    [],
  );
  return active ? shown : value;
}

// ---------------------------------------------------------------- Markdown

export interface MarkdownProps {
  /** The Markdown text exactly as received (never mutated; copying uses it). */
  source: string;
  /** The text is still arriving: unclosed fences and math stay plain, re-parse is throttled to frames. */
  streaming?: boolean;
  /** Offered on Python/R/Julia fences (e.g. "Run in Research"). */
  runAction?: CodeRunAction;
  /** How an allowed http(s) address opens; default: the system browser via `app_open_external`. */
  onOpenExternal?: (url: string) => void;
  className?: string;
  "data-testid"?: string;
}

/** Render untrusted Markdown safely (see the audit at the top of this file). */
export const Markdown = memo(function Markdown({
  source,
  streaming = false,
  runAction,
  onOpenExternal,
  className,
  "data-testid": testId,
}: MarkdownProps) {
  const shown = useFrameThrottled(source, streaming);
  const prepared = useMemo(() => normalizeMathDelimiters(shown), [shown]);
  const prefix = `${useId().replace(/[^a-zA-Z0-9_-]/g, "")}-`;
  const context = useMemo<RenderContext>(
    () => ({ source: prepared, streaming, runAction, openExternal: onOpenExternal ?? defaultOpenExternal }),
    [prepared, streaming, runAction, onOpenExternal],
  );
  const tree = useMemo(
    () => (
      <ReactMarkdown
        remarkPlugins={REMARK_PLUGINS}
        remarkRehypeOptions={{ clobberPrefix: prefix }}
        components={COMPONENTS}
        allowedElements={ALLOWED_ELEMENTS}
        unwrapDisallowed
        skipHtml
        urlTransform={urlTransform}
      >
        {prepared}
      </ReactMarkdown>
    ),
    [prepared, prefix],
  );
  return (
    <MarkdownContext.Provider value={context}>
      <div className={cx("ui-markdown", className)} data-streaming={streaming || undefined} data-testid={testId}>
        {tree}
      </div>
    </MarkdownContext.Provider>
  );
});

// ---------------------------------------------------------------- response surface

/**
 * Visual treatment of a response surface (pending the user's choice, A1):
 * `band` tinted band across the pane, `inset` tinted block on the pane inset,
 * `document` document-white surface with a hairline border.
 */
export type ResponseVariant = "band" | "inset" | "document";
export type ResponseDensity = "comfortable" | "compact";

export interface ResponseSurfaceProps {
  variant?: ResponseVariant;
  /** Default: compact inside panels and sidebars (StateSizeProvider "compact"), comfortable on pages. */
  density?: ResponseDensity;
  /** Who wrote it (shown as a group label), e.g. "Agent". */
  label?: ReactNode;
  /** The original Markdown; when given, the surface has a Copy action for it. */
  copyText?: string;
  /** What Copy copies, for its accessible name ("Copy response"). */
  copyWhat?: string;
  /** Status under the content (a streaming draft's activity). */
  footer?: ReactNode;
  children?: ReactNode;
  className?: string;
  "data-testid"?: string;
}

/** The surface that frames one response: label, Copy, content, optional status. */
export function ResponseSurface({
  variant = "inset",
  density,
  label,
  copyText,
  copyWhat = "response",
  footer,
  children,
  className,
  "data-testid": testId,
}: ResponseSurfaceProps) {
  const size = useStateSize();
  return (
    <div
      className={cx("ui-response", className)}
      data-variant={variant}
      data-density={density ?? (size === "compact" ? "compact" : "comfortable")}
      data-context={size === "compact" ? "region" : "page"}
      data-testid={testId}
    >
      {(label !== undefined || copyText !== undefined) && (
        <div className="ui-response__header">
          {label !== undefined && <span className="ui-group-label">{label}</span>}
          {copyText !== undefined && (
            <span className="ui-response__actions">
              <CopyButton text={copyText} what={copyWhat} testId="response-copy" />
            </span>
          )}
        </div>
      )}
      <div className="ui-response__body">{children}</div>
      {footer && <div className="ui-response__footer">{footer}</div>}
    </div>
  );
}
