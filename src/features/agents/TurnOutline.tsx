// The conversation as a turn outline (design C): your messages, then each
// agent turn under a header that summarizes what its tool calls and the
// review records recorded. The agent's text, reasoning summaries, notices,
// approvals, questions, and figures always stay visible in order; the
// routine tool-call rows show while the turn is expanded (a turn with a
// failure starts expanded). The Agent view and the Agents page share the
// expansion state, so switching views keeps it.
import { ChevronRight } from "lucide-react";
import { Fragment, type ReactNode } from "react";
import { create } from "zustand";

import type { ApprovalRequest } from "../../bindings/ApprovalRequest";
import type { FileReview } from "../../bindings/FileReview";
import type { TaskId } from "../../bindings/TaskId";
import { StatusText } from "../../ui/components";
import { ApprovalCard } from "./ApprovalCard";
import { WAITING_FOR_APPROVAL } from "./format";
import { FigureSlot, QuestionSlot } from "./slots";
import type { StreamDraft } from "./store";
import { Draft, ToolCallRow, TranscriptEntry } from "./Transcript";
import { groupTurns, isRoutine, QUESTION_TOOL, turnSummary, type Indexed, type ToolCallItem } from "./turns";
import type { TranscriptItem } from "../../bindings/TranscriptItem";

/** Turns whose expansion the reader flipped from its default (`<task>/<turn>`). */
const useToggled = create<{ toggled: Record<string, true> }>(() => ({ toggled: {} }));

function toggle(key: string) {
  useToggled.setState((s) => {
    const toggled = { ...s.toggled };
    if (toggled[key]) delete toggled[key];
    else toggled[key] = true;
    return { toggled };
  });
}

/** Anchor of an inline card that the "Needs you" list jumps to. */
export const approvalAnchor = (id: string) => `approval:${id}`;
export const questionAnchor = (toolUseId: string) => `question:${toolUseId}`;

function Anchor({ id, children }: { id: string; children: ReactNode }) {
  return (
    <div className="agents-anchor" data-needs-you={id}>
      {children}
    </div>
  );
}

function Approvals({ approvals }: { approvals: ApprovalRequest[] }) {
  return (
    <>
      {approvals.map((approval) => (
        <Anchor key={approval.id} id={approvalAnchor(approval.id)}>
          <ApprovalCard approval={approval} />
        </Anchor>
      ))}
    </>
  );
}

/** Consecutive entries: either a run of routine tool calls or one visible entry. */
type Block = { kind: "calls"; key: string; calls: Indexed<ToolCallItem>[] } | { kind: "entry"; entry: Indexed };

function blocks(entries: Indexed[]): Block[] {
  const out: Block[] = [];
  for (const entry of entries) {
    if (isRoutine(entry.item)) {
      const last = out[out.length - 1];
      const call = entry as Indexed<ToolCallItem>;
      if (last?.kind === "calls") last.calls.push(call);
      else out.push({ kind: "calls", key: `calls-${entry.index}`, calls: [call] });
    } else out.push({ kind: "entry", entry });
  }
  return out;
}

function AgentTurn({
  taskId,
  turnKey,
  entries,
  approvalsFor,
  reviews,
  tail,
}: {
  taskId: TaskId;
  turnKey: string;
  entries: Indexed[];
  approvalsFor: (toolUseId: string) => ApprovalRequest[];
  reviews: ReadonlyMap<string, FileReview>;
  /** The streaming draft and unplaced approvals (last turn only). */
  tail?: ReactNode;
}) {
  const key = `${taskId}/${turnKey}`;
  const flipped = useToggled((s) => s.toggled[key] === true);
  const calls = entries.filter((e): e is Indexed<ToolCallItem> => isRoutine(e.item)).map((e) => e.item);
  const summary = turnSummary(calls, reviews);
  const failed = summary.failed > 0 || entries.some((e) => e.item.kind === "notice" && e.item.level === "error");
  const open = calls.length > 0 && flipped !== failed;
  const parts = blocks(entries);
  const runIds = parts.filter((b) => b.kind === "calls").map((b) => `${taskId}-${b.key}`);
  return (
    <section className="agents-turn" data-role="agent" data-testid="agents-turn" data-open={open || undefined} data-failed={failed || undefined}>
      {calls.length > 0 && (
        <button
          type="button"
          className="agents-turn__toggle"
          aria-expanded={open}
          aria-controls={open ? runIds.join(" ") : undefined}
          onClick={() => toggle(key)}
          data-testid="agents-turn-toggle"
        >
          <ChevronRight size={14} aria-hidden className="agents-turn__chevron" />
          <span className="agents-turn__summary" data-testid="agents-turn-summary">
            {summary.text}
          </span>
          {summary.failed > 0 && <StatusText status="failed" label={`${summary.failed} failed`} />}
          {summary.waiting > 0 && <StatusText status="warning" label={WAITING_FOR_APPROVAL} />}
          {summary.declined > 0 && <StatusText status="canceled" label={`${summary.declined} declined`} />}
          {summary.running && <StatusText status="running" label={`Running: ${summary.running}`} />}
        </button>
      )}
      {parts.map((block) => {
        if (block.kind === "calls") {
          return (
            <Fragment key={block.key}>
              {open && (
                <div className="agents-turn__activity" id={`${taskId}-${block.key}`} role="group" aria-label="Agent activity">
                  {block.calls.map(({ item, index }) => (
                    <ToolCallRow key={`${item.seq}-${index}`} item={item} />
                  ))}
                </div>
              )}
              {block.calls.map(({ item, index }) => (
                <Fragment key={`${item.seq}-${index}`}>
                  <Approvals approvals={approvalsFor(item.toolUseId)} />
                  <FigureSlot taskId={taskId} item={item} />
                </Fragment>
              ))}
            </Fragment>
          );
        }
        const { item, index } = block.entry;
        if (item.kind === "toolCall" && item.name === QUESTION_TOOL) {
          return (
            <Fragment key={`${item.seq}-${index}`}>
              <Anchor id={questionAnchor(item.toolUseId)}>
                <QuestionSlot taskId={taskId} item={item} />
              </Anchor>
              <Approvals approvals={approvalsFor(item.toolUseId)} />
            </Fragment>
          );
        }
        return <TranscriptEntry key={`${item.seq}-${index}`} item={item} />;
      })}
      {tail}
    </section>
  );
}

/**
 * The outline of one task's conversation. Approvals render at their tool
 * call (or at the end when the call is not recorded yet); the streaming
 * draft closes the last agent turn.
 */
export function TurnOutline({
  taskId,
  transcript,
  approvals,
  draft,
  reviews,
}: {
  taskId: TaskId;
  transcript: readonly TranscriptItem[];
  approvals: readonly ApprovalRequest[];
  draft: StreamDraft | null;
  reviews: ReadonlyMap<string, FileReview>;
}) {
  const groups = groupTurns(transcript);
  const calls = new Set(transcript.flatMap((item) => (item.kind === "toolCall" ? [item.toolUseId] : [])));
  const approvalsFor = (toolUseId: string) => approvals.filter((a) => a.toolUseId === toolUseId);
  const unplaced = approvals.filter((a) => !calls.has(a.toolUseId));
  const tail =
    draft || unplaced.length ? (
      <>
        <Approvals approvals={unplaced} />
        {draft && <Draft draft={draft} />}
      </>
    ) : null;
  const last = groups[groups.length - 1];
  return (
    <div className="agents-outline">
      {groups.map((group) =>
        group.kind === "user" ? (
          <div key={group.key} className="agents-turn" data-role="user">
            <TranscriptEntry item={group.entry.item} />
          </div>
        ) : (
          <AgentTurn
            key={group.key}
            taskId={taskId}
            turnKey={group.key}
            entries={group.entries}
            approvalsFor={approvalsFor}
            reviews={reviews}
            tail={group === last ? tail : undefined}
          />
        ),
      )}
      {last?.kind !== "agent" && tail && (
        <section className="agents-turn" data-role="agent" data-testid="agents-turn">
          {tail}
        </section>
      )}
    </div>
  );
}

/** Tests only. */
export function resetOutlineForTests() {
  useToggled.setState({ toggled: {} });
}
