// "Needs you" above the composer (design C): jump links to the pending
// approvals and questions, which stay inline at their action point in the
// conversation, and — once the task has stopped with changes to review —
// "Review changes: N files · M unsaved". Nothing is answered from here.
import { ArrowUp, ChevronRight, FileDiff, MessageCircleQuestion, ShieldAlert } from "lucide-react";
import { useRef, type KeyboardEvent, type ReactNode } from "react";

import type { ApprovalRequest } from "../../bindings/ApprovalRequest";
import type { QuestionRequest } from "../../bindings/QuestionRequest";
import { ListRow } from "../../ui/components";
import { WAITING_FOR_APPROVAL } from "./format";
import { jumpToQuestion, needsYouLabel } from "./questions";
import { approvalAnchor } from "./TurnOutline";

/**
 * Scroll an inline card into view and move focus to it (the approval card's
 * label names the decision; its buttons are the next Tab stops).
 */
export function jumpTo(container: HTMLElement | null, anchor: string): boolean {
  const target = container?.querySelector<HTMLElement>(`[data-needs-you="${anchor.replace(/["\\]/g, "\\$&")}"]`);
  if (!target) return false;
  const card = target.querySelector<HTMLElement>("[data-testid=agents-approval]") ?? target;
  if (!card.hasAttribute("tabindex")) card.setAttribute("tabindex", "-1");
  card.scrollIntoView?.({ block: "center" });
  card.focus({ preventScroll: true });
  return true;
}

function JumpRow({ icon, label, trailing, onActivate, testId }: { icon: ReactNode; label: string; trailing: ReactNode; onActivate: () => void; testId: string }) {
  return (
    <ListRow
      role="link"
      tabIndex={0}
      aria-label={label}
      className="agents-needs__row"
      icon={icon}
      title={label}
      trailing={trailing}
      onClick={onActivate}
      onKeyDown={(event: KeyboardEvent<HTMLDivElement>) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onActivate();
        }
      }}
      data-testid={testId}
    />
  );
}

export function NeedsYou({
  approvals,
  questions,
  review,
  onJump,
  onReview,
}: {
  approvals: readonly ApprovalRequest[];
  /** Open question requests (`pendingQuestions(detail)`, questions.ts). */
  questions: readonly QuestionRequest[];
  /** Files to review after the task stopped (`null`: nothing to review now). */
  review: { files: number; unsaved: number } | null;
  onJump: (anchor: string) => void;
  onReview: () => void;
}) {
  const navRef = useRef<HTMLElement>(null);
  const count = approvals.length + questions.length + (review ? 1 : 0);
  if (count === 0) return null;
  const jump = (
    <span className="ui-meta agents-needs__jump">
      Jump <ArrowUp size={12} aria-hidden />
    </span>
  );
  return (
    <nav ref={navRef} className="agents-needs" aria-label="Needs you" data-testid="agents-needs-you">
      <span className="ui-group-label agents-needs__label">
        Needs you <span className="ui-section__count">{count}</span>
      </span>
      {questions.map((q) => (
        <JumpRow
          key={q.id}
          icon={<MessageCircleQuestion size={14} aria-hidden />}
          label={needsYouLabel(q)}
          trailing={jump}
          onActivate={() => jumpToQuestion(q.id, navRef.current)}
          testId="agents-needs-question"
        />
      ))}
      {approvals.map((a) => (
        <JumpRow
          key={a.id}
          icon={<ShieldAlert size={14} aria-hidden />}
          label={`${WAITING_FOR_APPROVAL}: ${a.title}`}
          trailing={jump}
          onActivate={() => onJump(approvalAnchor(a.id))}
          testId="agents-needs-approval"
        />
      ))}
      {review && (
        <JumpRow
          icon={<FileDiff size={14} aria-hidden />}
          label={`Review changes: ${review.files} file${review.files === 1 ? "" : "s"}${review.unsaved ? ` · ${review.unsaved} unsaved` : ""}`}
          trailing={<ChevronRight size={14} aria-hidden />}
          onActivate={onReview}
          testId="agents-needs-review"
        />
      )}
    </nav>
  );
}
