// The conversation as a turn outline (pure; unit tested). Transcript items are
// grouped into your messages and agent turns (everything the agent recorded
// between two of your messages). A turn's header summarizes what its tool
// calls recorded and what the review records say about the files — never the
// assistant's prose — so a claim in the text cannot become a claimed action.
import type { FileReview } from "../../bindings/FileReview";
import type { TaskStatus } from "../../bindings/TaskStatus";
import type { TranscriptItem } from "../../bindings/TranscriptItem";

export type ToolCallItem = Extract<TranscriptItem, { kind: "toolCall" }>;
export type UserTextItem = Extract<TranscriptItem, { kind: "userText" }>;

/** An item with its position in the transcript (positions are stable: the transcript only grows). */
export interface Indexed<T extends TranscriptItem = TranscriptItem> {
  item: T;
  index: number;
}

export type OutlineGroup =
  | { kind: "user"; key: string; entry: Indexed<UserTextItem> }
  | { kind: "agent"; key: string; entries: Indexed[] };

/** Your messages and the agent turns between them, in order. */
export function groupTurns(items: readonly TranscriptItem[]): OutlineGroup[] {
  const groups: OutlineGroup[] = [];
  items.forEach((item, index) => {
    if (item.kind === "userText") {
      groups.push({ kind: "user", key: `user-${index}`, entry: { item, index } });
      return;
    }
    const last = groups[groups.length - 1];
    if (last?.kind === "agent") last.entries.push({ item, index });
    else groups.push({ kind: "agent", key: `turn-${index}`, entries: [{ item, index }] });
  });
  return groups;
}

/** The tool name of a structured question (rendered by the question card, never as routine activity). */
export const QUESTION_TOOL = "ask_user";

/** Figure references recorded on a tool call (added by the backend; absent on older transcripts). */
export function figuresOf(item: ToolCallItem): unknown[] {
  const figures = (item as ToolCallItem & { figures?: unknown }).figures;
  return Array.isArray(figures) ? figures : [];
}

/** A tool call that is routine activity (collapses under the turn header). */
export function isRoutine(item: TranscriptItem): item is ToolCallItem {
  return item.kind === "toolCall" && item.name !== QUESTION_TOOL;
}

/** Saved / unsaved / reverted, from the review record (`unsaved` arrives from the backend; optional). */
export function reviewSuffix(review: FileReview | undefined): string | null {
  if (!review) return null;
  if (review.state === "reverted") return "reverted";
  if (review.state === "undone") return "undone";
  const unsaved = (review as FileReview & { unsaved?: boolean | null }).unsaved;
  if (unsaved === true) return "unsaved";
  if (unsaved === false) return "saved";
  return null;
}

export interface TurnSummary {
  /** "Read 3 files · Edited main.tex — unsaved · Ran Python · 1 figure" (empty when the turn has no tool calls). */
  text: string;
  failed: number;
  waiting: number;
  declined: number;
  /** The tool still running, if any. */
  running: string | null;
  /** Routine tool calls in the turn. */
  calls: number;
}

const str = (v: unknown) => (typeof v === "string" ? v : "");
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const fileName = (path: string) => path.split(/[\\/]/).pop() || path;

function inputOf(item: ToolCallItem): Record<string, unknown> {
  return typeof item.input === "object" && item.input !== null ? (item.input as Record<string, unknown>) : {};
}

function pathCount(item: ToolCallItem): number {
  const paths = inputOf(item).paths;
  return Array.isArray(paths) ? Math.max(1, paths.length) : 1;
}

/** "Edited main.tex — unsaved" for one file, "Edited 3 files · 2 unsaved" for several. */
function filesPart(verb: string, paths: string[], reviews: ReadonlyMap<string, FileReview>): string {
  if (paths.length === 1) {
    const suffix = reviewSuffix(reviews.get(paths[0]!));
    return `${verb} ${fileName(paths[0]!)}${suffix ? ` — ${suffix}` : ""}`;
  }
  const unsaved = paths.filter((p) => reviewSuffix(reviews.get(p)) === "unsaved").length;
  return `${verb} ${plural(paths.length, "file")}${unsaved ? ` · ${unsaved} unsaved` : ""}`;
}

/**
 * What a turn's tool calls recorded. Only calls that succeeded count as done
 * ("Read", "Edited", "Ran"); pending, waiting, declined, and canceled calls
 * never read as completed — waiting, running, failed, and declined are
 * returned as counts for status text.
 */
export function turnSummary(calls: readonly ToolCallItem[], reviews: ReadonlyMap<string, FileReview> = new Map()): TurnSummary {
  const done = calls.filter((c) => c.status === "succeeded");
  const unique = (names: string[]) => [...new Set(done.filter((c) => names.includes(c.name)).map((c) => str(inputOf(c).path)).filter(Boolean))];
  const count = (name: string) => done.filter((c) => c.name === name).length;
  const sum = (name: string) => done.filter((c) => c.name === name).reduce((n, c) => n + pathCount(c), 0);
  const known = new Set<string>();
  const parts: (string | null)[] = [];
  const add = (names: string[], part: string | null) => {
    names.forEach((n) => known.add(n));
    parts.push(part);
  };

  const reads = unique(["read_file"]);
  add(["read_file"], reads.length === 1 ? `Read ${fileName(reads[0]!)}` : reads.length ? `Read ${plural(reads.length, "file")}` : null);
  add(["list_files"], count("list_files") ? "Listed files" : null);
  add(["graph_query"], count("graph_query") ? "Queried the graph" : null);
  const edited = unique(["edit_document"]);
  add(["edit_document"], edited.length ? filesPart("Edited", edited, reviews) : null);
  const created = unique(["create_file"]);
  add(["create_file"], created.length ? filesPart("Created", created, reviews) : null);
  const saved = unique(["save_document"]);
  add(["save_document"], saved.length === 1 ? `Saved ${fileName(saved[0]!)}` : saved.length ? `Saved ${plural(saved.length, "file")}` : null);
  add(["copy_paths"], sum("copy_paths") ? `Copied ${plural(sum("copy_paths"), "item")}` : null);
  add(["delete_paths"], sum("delete_paths") ? `Deleted ${plural(sum("delete_paths"), "item")}` : null);
  add(["run_python"], count("run_python") ? "Ran Python" : null);
  add(["run_command"], count("run_command") ? (count("run_command") === 1 ? "Ran a command" : `Ran ${count("run_command")} commands`) : null);
  add(["install_packages"], count("install_packages") ? "Installed packages" : null);
  const built = unique(["build_document"]);
  add(["build_document"], count("build_document") ? (built.length === 1 ? `Built ${fileName(built[0]!)}` : `Built ${plural(count("build_document"), "document")}`) : null);
  add(["check_lean"], count("check_lean") ? `Checked ${plural(count("check_lean"), "file")} in Lean` : null);
  add(["insert_artifact"], count("insert_artifact") ? `Inserted ${plural(count("insert_artifact"), "result")}` : null);
  add(["graph_propose"], count("graph_propose") ? "Proposed graph changes" : null);
  add(["workflow_propose"], count("workflow_propose") ? "Proposed a workflow" : null);
  const figures = calls.reduce((n, c) => n + figuresOf(c).length, 0);
  parts.push(figures ? plural(figures, "figure") : null);
  const other = done.filter((c) => !known.has(c.name)).length;
  parts.push(other ? plural(other, "other action") : null);

  const text = parts.filter((p): p is string => p !== null).join(" · ");
  const running = calls.find((c) => c.status === "running" || c.status === "pending");
  return {
    text: text || (calls.length ? plural(calls.length, "action") : ""),
    failed: calls.filter((c) => c.status === "failed").length,
    waiting: calls.filter((c) => c.status === "awaitingApproval").length,
    declined: calls.filter((c) => c.status === "declined").length,
    running: running ? running.name : null,
    calls: calls.length,
  };
}

/**
 * The transcript as the conversation shows it: when a failed task's last
 * entry is an error notice with exactly the failure message, the "Task
 * failed" banner under the conversation states it, so it is not repeated
 * right above the banner. Everything else is shown as recorded.
 */
export function withoutRepeatedFailure(items: readonly TranscriptItem[], status: TaskStatus): readonly TranscriptItem[] {
  if (status.kind !== "failed") return items;
  const last = items[items.length - 1];
  if (last?.kind === "notice" && last.level === "error" && last.message.trim() === status.message.trim()) return items.slice(0, -1);
  return items;
}
