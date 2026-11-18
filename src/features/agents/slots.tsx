// Hook points of the turn outline for cards another lane adds:
//
// - `QuestionSlot` renders a tool call named `ask_user` (a structured
//   question). It always stays visible in its turn, at the point where the
//   agent asked, and is the jump target of the "Needs you" list
//   (`data-needs-you` = the tool use id). Until the question card exists, the
//   call is shown as an ordinary tool-call row.
// - `FigureSlot` renders the figures recorded on a tool call (`figures`),
//   right after the call, outside the collapsible activity (figures stay
//   visible). Until the figure card exists, it renders nothing.
//
// Replace the bodies; keep the props.
import type { TaskId } from "../../bindings/TaskId";
import { ToolCallRow } from "./Transcript";
import { figuresOf, type ToolCallItem } from "./turns";

export interface SlotProps {
  taskId: TaskId;
  item: ToolCallItem;
}

export function QuestionSlot({ item }: SlotProps) {
  return <ToolCallRow item={item} />;
}

export function FigureSlot({ item }: SlotProps) {
  if (figuresOf(item).length === 0) return null;
  return null;
}
