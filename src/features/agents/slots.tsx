// Cards of the turn outline:
//
// - `QuestionSlot` renders a tool call named `ask_user` (a structured
//   question) as its `QuestionCard`. It always stays visible in its turn, at
//   the point where the agent asked, and is the jump target of the "Needs
//   you" list. Without a recorded request (an older transcript, or the call
//   before the request arrives) the call is shown as an ordinary tool-call row.
// - `FigureSlot` renders the figures recorded on a tool call (`figures`),
//   right after the call, outside the collapsible activity (figures stay
//   visible).
import { lazy, Suspense } from "react";

import type { TaskId } from "../../bindings/TaskId";
import { QuestionCard } from "./QuestionCard";
import { questionsForTask } from "./questions";
import { useAgents } from "./store";
import { ToolCallRow } from "./Transcript";
import type { ToolCallItem } from "./turns";

// The figure card reaches Research's promotion flow (and through it the
// editor); it loads with the first figure, not with every conversation.
const FigureCard = lazy(() => import("./FigureCard").then((m) => ({ default: m.FigureCard })));

export interface SlotProps {
  taskId: TaskId;
  item: ToolCallItem;
}

export function QuestionSlot({ taskId, item }: SlotProps) {
  const request = useAgents((s) => questionsForTask(s.details[taskId]).get(item.toolUseId) ?? null);
  if (!request) return <ToolCallRow item={item} />;
  return <QuestionCard task={taskId} request={request} />;
}

export function FigureSlot({ taskId, item }: SlotProps) {
  const figures = item.figures ?? [];
  if (figures.length === 0) return null;
  return (
    <Suspense fallback={null}>
      {figures.map((figure) => (
        <FigureCard key={`${figure.runId}/${figure.artifactId}`} task={taskId} figure={figure} />
      ))}
    </Suspense>
  );
}
