// Research view model: pure functions over the Rust research types, so the
// event reducer, status vocabulary, and attachment helpers are unit-testable.
import { formatDuration as formatElapsed } from "../../lib/format";
import type { ArtifactRecord } from "../../bindings/ArtifactRecord";
import type { OutputEntry } from "../../bindings/OutputEntry";
import type { ResearchEvent } from "../../bindings/ResearchEvent";
import type { RunManifest } from "../../bindings/RunManifest";
import type { RunSummary } from "../../bindings/RunSummary";
import type { SessionInfo } from "../../bindings/SessionInfo";
import type { Status } from "../../ui/components";

/** Output entries kept per run in the webview (Rust keeps its own bounded copy). */
export const MAX_ENTRIES = 2000;
/** Text kept per run in the webview; older entries are dropped first (history pages from Rust). */
export const MAX_RETAINED_CHARS = 512 * 1024;
/** Characters of one entry rendered before "Show more". */
export const ENTRY_PREVIEW_CHARS = 4 * 1024;

export interface RunOutput {
  entries: OutputEntry[];
  lastSeq: number;
  /** Older entries were dropped here or in Rust; page from the server for history. */
  truncated: boolean;
  /** The output has been loaded from the server at least once (history runs). */
  loaded: boolean;
}

export const EMPTY_RUN_OUTPUT: RunOutput = { entries: [], lastSeq: 0, truncated: false, loaded: false };

function entryChars(entry: OutputEntry): number {
  const item = entry.item;
  switch (item.kind) {
    case "stream":
      return item.text.length;
    case "display":
      return (item.text?.length ?? 0) + 64;
    case "error":
      return item.evalue.length + item.traceback.reduce((n, line) => n + line.length, 0);
    case "notice":
      return item.message.length;
    case "clearOutput":
      return 8;
  }
}

export interface ResearchModel {
  sessions: SessionInfo[];
  runs: RunSummary[];
  outputs: Record<string, RunOutput>;
}

/** Append entries in sequence order, skipping duplicates and honoring clear_output. */
export function appendEntries(output: RunOutput, entries: OutputEntry[]): RunOutput {
  let next = output.entries;
  let lastSeq = output.lastSeq;
  let truncated = output.truncated;
  let changed = false;
  for (const entry of entries) {
    if (entry.seq <= lastSeq) continue;
    lastSeq = entry.seq;
    changed = true;
    if (entry.item.kind === "clearOutput") {
      // Keep supervisor notices; clear kernel output.
      next = next.filter((e) => e.item.kind === "notice");
      continue;
    }
    next = [...next, entry];
  }
  if (!changed) return output;
  if (next.length > MAX_ENTRIES) {
    next = next.slice(next.length - MAX_ENTRIES);
    truncated = true;
  }
  let chars = 0;
  for (let i = next.length - 1; i >= 0; i -= 1) {
    chars += entryChars(next[i] as OutputEntry);
    if (chars > MAX_RETAINED_CHARS && i < next.length - 1) {
      next = next.slice(i + 1);
      truncated = true;
      break;
    }
  }
  return { ...output, entries: next, lastSeq, truncated };
}

function upsert<T>(items: T[], item: T, same: (a: T) => boolean, prepend = false): T[] {
  const index = items.findIndex(same);
  if (index < 0) return prepend ? [item, ...items] : [...items, item];
  const copy = items.slice();
  copy[index] = item;
  return copy;
}

/**
 * Combine a snapshot read from the service with the current list. An item an
 * event changed (or removed, or added) while the snapshot was being fetched is
 * newer than the snapshot: the current version wins. Every service change is
 * followed by its event, so the list still converges on the service's state.
 */
export function mergeSnapshot<T>(
  current: T[],
  snapshot: T[],
  idOf: (item: T) => string,
  changedDuringFetch: (id: string) => boolean,
): T[] {
  const merged: T[] = [];
  for (const item of snapshot) {
    const id = idOf(item);
    if (!changedDuringFetch(id)) merged.push(item);
    else {
      const now = current.find((c) => idOf(c) === id);
      if (now) merged.push(now);
    }
  }
  for (const item of current) {
    const id = idOf(item);
    if (changedDuringFetch(id) && !merged.some((m) => idOf(m) === id)) merged.push(item);
  }
  return merged;
}

/** History order: newest start first, then id (descending), as the service lists a page. */
function byStartDescending(a: RunSummary, b: RunSummary): number {
  if (a.startedAtMs !== b.startedAtMs) return b.startedAtMs - a.startedAtMs;
  return a.id < b.id ? 1 : a.id > b.id ? -1 : 0;
}

/**
 * Add a further page of the run history to the runs already listed. A run
 * listed already keeps the version held here (an event may have changed it
 * since the page was read); the result is in history order.
 */
export function appendRunPage(current: RunSummary[], page: RunSummary[]): RunSummary[] {
  const listed = new Set(current.map((r) => r.id));
  const added = page.filter((r) => !listed.has(r.id));
  if (added.length === 0) return current;
  return [...current, ...added].sort(byStartDescending);
}

/**
 * The history entry of a stored run read on its own (`research_run`), for a
 * run that is on a page of the history not loaded here. Mirrors the summary
 * the service lists for a stored run.
 */
export function summaryOfManifest(manifest: RunManifest): RunSummary {
  const text = manifest.code.text;
  const codePreview =
    text === null || text === undefined
      ? `sha256:${manifest.code.sha256.slice(0, 12)}`
      : Array.from(text.split(/\r?\n/).find((line) => line.trim() !== "") ?? "").slice(0, 120).join("");
  return {
    id: manifest.record.id,
    sessionId: manifest.session.sessionId,
    owner: manifest.session.owner,
    status: manifest.record.status,
    queued: false,
    origin: manifest.code.origin,
    codePreview,
    executionCount: manifest.executionCount,
    startedAtMs: manifest.record.startedAtMs,
    finishedAtMs: manifest.record.finishedAtMs,
    outputs: manifest.record.outputs,
    error: manifest.error,
  };
}

export function applyEvent(model: ResearchModel, event: ResearchEvent): ResearchModel {
  switch (event.kind) {
    case "sessionChanged":
      return { ...model, sessions: upsert(model.sessions, event.session, (s) => s.id === event.session.id) };
    case "sessionRemoved":
      return { ...model, sessions: model.sessions.filter((s) => s.id !== event.sessionId) };
    case "runQueued":
    case "runFinished":
      return { ...model, runs: upsert(model.runs, event.run, (r) => r.id === event.run.id, true) };
    case "runStarted":
      return {
        ...model,
        runs: model.runs.map((r) => (r.id === event.runId ? { ...r, queued: false, executionCount: event.executionCount } : r)),
      };
    case "output": {
      // A finished run whose output is not held here (never selected, or
      // released by `pruneOutputs`) is read whole from the service when it
      // is selected; a stray late entry must not stand in for that read.
      if (!model.outputs[event.runId] && model.runs.some((r) => r.id === event.runId && r.status.kind !== "running")) return model;
      const current = model.outputs[event.runId] ?? { ...EMPTY_RUN_OUTPUT, loaded: true };
      const next = appendEntries(current, [event.entry]);
      if (next === current) return model;
      return { ...model, outputs: { ...model.outputs, [event.runId]: next } };
    }
  }
}

/** Outputs of finished runs kept in the webview besides the selected run's. */
export const KEPT_FINISHED_OUTPUTS = 8;

/**
 * Release the output of finished runs beyond the newest `keep` (the selected
 * run and runs still executing always keep theirs; a run that is not in
 * `runs` counts as finished). The service holds every
 * run's output (its own recent copy, or the stored transcript) and selecting
 * a run whose output is not held here reads it again, so nothing is lost:
 * only the copies kept in this window are bounded.
 */
export function pruneOutputs(
  outputs: Record<string, RunOutput>,
  runs: RunSummary[],
  selectedRun: string | null,
  keep = KEPT_FINISHED_OUTPUTS,
): Record<string, RunOutput> {
  const started = new Map<string, number>();
  const running = new Set<string>();
  for (const run of runs) {
    if (run.status.kind === "running") running.add(run.id);
    else started.set(run.id, run.startedAtMs);
  }
  // A run that is not listed (read by id from a page not loaded, or gone
  // from the history) is a finished run too.
  const held = Object.keys(outputs);
  const finished = held.filter((id) => id !== selectedRun && !running.has(id));
  if (finished.length <= keep) return outputs;
  // Listed runs by start time; then runs not listed, the one held last first.
  const order = new Map(held.map((id, index) => [id, index]));
  finished.sort((a, b) => {
    const [x, y] = [started.get(a), started.get(b)];
    if (x !== undefined && y !== undefined) return y - x;
    if (x !== undefined) return -1;
    if (y !== undefined) return 1;
    return order.get(b)! - order.get(a)!;
  });
  const released = new Set(finished.slice(keep));
  const next: Record<string, RunOutput> = {};
  for (const [id, output] of Object.entries(outputs)) if (!released.has(id)) next[id] = output;
  return next;
}

export interface PillState {
  status: Status;
  label: string;
}

export function runPill(run: RunSummary): PillState {
  switch (run.status.kind) {
    case "running":
      return run.queued ? { status: "idle", label: "Queued" } : { status: "running", label: "Running" };
    case "succeeded":
      return { status: "succeeded", label: "Executed" };
    case "failed":
      return { status: "failed", label: run.error ? run.error.ename : "Failed" };
    case "interrupted":
      return { status: "canceled", label: "Interrupted" };
    case "canceled":
      return { status: "canceled", label: "Canceled" };
  }
}

export function sessionPill(session: SessionInfo | undefined): PillState {
  if (!session) return { status: "idle", label: "No session" };
  switch (session.state.kind) {
    case "starting":
      return { status: "running", label: "Starting kernel" };
    case "idle":
      return { status: "idle", label: "Kernel idle" };
    case "busy":
      return { status: "running", label: "Kernel busy" };
    case "restarting":
      return { status: "running", label: "Restarting kernel" };
    case "crashed":
      return { status: "failed", label: "Kernel crashed" };
    case "detached":
      return { status: "stale", label: "Session detached" };
    case "stopped":
      return { status: "canceled", label: "Session stopped" };
  }
}

export function isActiveRun(run: RunSummary): boolean {
  return run.status.kind === "running";
}

export type DocumentFormat = "typst" | "latex" | "markdown";

export function documentFormat(path: string | null | undefined): DocumentFormat | null {
  const ext = path?.split(".").pop()?.toLowerCase();
  if (ext === "typ") return "typst";
  if (ext === "tex" || ext === "ltx") return "latex";
  if (ext === "md" || ext === "markdown") return "markdown";
  return null;
}

/**
 * Default insert position (UTF-16 offset, i.e. a JS string index): the end of
 * the document, or the start of the `\end{document}` line for LaTeX.
 */
export function defaultInsertOffset(text: string, format: DocumentFormat): number {
  if (format === "latex") {
    const end = text.lastIndexOf("\\end{document}");
    if (end >= 0) {
      const lineStart = text.lastIndexOf("\n", end - 1) + 1;
      return text.slice(lineStart, end).trim() === "" ? lineStart : end;
    }
  }
  return text.length;
}

const MARKER: Record<DocumentFormat, RegExp> = {
  typst: /^[ \t]*\/\/ exposition:begin (\S+)[ \t]*\r?$/gm,
  latex: /^[ \t]*% exposition:begin (\S+)[ \t]*\r?$/gm,
  markdown: /^[ \t]*<!-- exposition:begin (\S+) -->[ \t]*\r?$/gm,
};

/** Attachment keys whose begin markers appear in the document. */
export function attachmentKeys(text: string, format: DocumentFormat): string[] {
  const keys: string[] = [];
  for (const match of text.matchAll(MARKER[format])) {
    const key = match[1];
    if (key && !keys.includes(key)) keys.push(key);
  }
  return keys;
}

export type AttachmentKind = "figure" | "table";

/** Text between `opener` and its matching `close`, skipping backslash escapes. */
function balanced(text: string, opener: string, open: string, close: string): { start: number; end: number } | null {
  const at = text.indexOf(opener);
  if (at < 0) return null;
  const start = at + opener.length;
  let depth = 1;
  for (let i = start; i < text.length; i += 1) {
    const ch = text[i];
    if (ch === "\\") i += 1;
    else if (ch === open) depth += 1;
    else if (ch === close && --depth === 0) return { start: at, end: i + 1 };
  }
  return null;
}

/** The text of the block with `key` (markers included), or null. */
function attachmentBlock(text: string, format: DocumentFormat, key: string): string | null {
  const lines = text.split("\n").map((l) => l.replace(/\r$/, "").trim());
  const marker = (word: string) =>
    format === "typst"
      ? `// exposition:${word} ${key}`
      : format === "latex"
        ? `% exposition:${word} ${key}`
        : `<!-- exposition:${word} ${key} -->`;
  const begin = lines.indexOf(marker("begin"));
  const end = lines.indexOf(marker("end"), begin + 1);
  return begin < 0 || end < 0 ? null : lines.slice(begin, end + 1).join("\n");
}

/**
 * Whether the attachment `key` shows a figure or a table. Mirrors the attach
 * service: the block's content decides (keys are free text and users edit
 * inside blocks); the generated `fig-`/`tab-` prefix only when the content is
 * unrecognizable; null when neither tells.
 */
export function attachmentKind(text: string, format: DocumentFormat, key: string): AttachmentKind | null {
  const block = attachmentBlock(text, format, key);
  let figure = false;
  let table = false;
  if (block !== null) {
    if (format === "typst") {
      // The caption is user text: "a table(1)" in it says nothing.
      const caption = balanced(block, "caption: [", "[", "]");
      const body = caption ? block.slice(0, caption.start) + block.slice(caption.end) : block;
      figure = body.includes("image(");
      table = body.includes("table(");
    } else if (format === "latex") {
      for (const line of block.split("\n")) {
        const code = line.replace(/(^|[^\\])%.*$/, "$1");
        const f = code.indexOf("\\begin{figure");
        const t = Math.max(code.indexOf("\\begin{table}"), code.indexOf("\\begin{table*}"));
        if (f >= 0 || t >= 0) {
          figure = f >= 0 && (t < 0 || f < t);
          table = !figure;
          break;
        }
      }
    } else {
      for (const line of block.split("\n").slice(1, -1)) {
        figure ||= line.startsWith("![");
        table ||= line.startsWith("|");
      }
    }
  }
  if (figure !== table) return figure ? "figure" : "table";
  if (key.startsWith("fig-")) return "figure";
  if (key.startsWith("tab-")) return "table";
  return null;
}

/** Attachments that an artifact of `kind` may update (same kind only), in document order. */
export function attachmentsOfKind(text: string, format: DocumentFormat, kind: AttachmentKind): string[] {
  return attachmentKeys(text, format).filter((key) => attachmentKind(text, format, key) === kind);
}

/** Figure/table artifacts referenced by one display output. */
export function displayArtifacts(run: RunSummary | undefined, ids: string[]): {
  figure?: ArtifactRecord;
  table?: ArtifactRecord;
  html?: ArtifactRecord;
} {
  const records = (run?.outputs ?? []).filter((a) => ids.includes(a.id));
  return {
    figure: records.find((a) => a.kind === "figure"),
    table: records.find((a) => a.kind === "table"),
    html: records.find((a) => a.mediaType === "text/html"),
  };
}

export function formatDuration(run: RunSummary, now: number): string | undefined {
  const end = run.finishedAtMs ?? (isActiveRun(run) && !run.queued ? now : null);
  if (end === null) return undefined;
  return formatElapsed(end - run.startedAtMs);
}

/** Strip terminal escape sequences that some kernels leave in text. */
export function stripAnsi(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "");
}
