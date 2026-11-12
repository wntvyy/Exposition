// Research view model: pure functions over the Rust research types, so the
// event reducer, status vocabulary, and attachment helpers are unit-testable.
import type { ArtifactRecord } from "../../bindings/ArtifactRecord";
import type { OutputEntry } from "../../bindings/OutputEntry";
import type { ResearchEvent } from "../../bindings/ResearchEvent";
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
      const current = model.outputs[event.runId] ?? { ...EMPTY_RUN_OUTPUT, loaded: true };
      const next = appendEntries(current, [event.entry]);
      if (next === current) return model;
      return { ...model, outputs: { ...model.outputs, [event.runId]: next } };
    }
  }
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

export function formatClock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

export function formatDuration(run: RunSummary, now: number): string | undefined {
  const end = run.finishedAtMs ?? (isActiveRun(run) && !run.queued ? now : null);
  if (end === null) return undefined;
  const seconds = Math.max(0, (end - run.startedAtMs) / 1000);
  return seconds < 10 ? `${seconds.toFixed(1)}s` : `${Math.round(seconds)}s`;
}

export function formatBytes(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KiB`;
  return `${(size / 1024 / 1024).toFixed(1)} MiB`;
}

/** Strip terminal escape sequences that some kernels leave in text. */
export function stripAnsi(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "");
}
